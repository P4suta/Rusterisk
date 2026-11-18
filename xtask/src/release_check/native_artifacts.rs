use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

const MAX_FILES: usize = 8192;
const MAX_ENTRIES: usize = 16_384;
const MAX_FILE_BYTES: u64 = 272 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 4096;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn physical_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || !fs::symlink_metadata(path)?.is_dir()
        || fs::canonicalize(path)? != path
    {
        return Err(invalid(
            "native artifacts require physical absolute directories",
        ));
    }
    Ok(())
}

fn relative(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '\\' | ':'))
    {
        return Err(invalid(
            "native artifact inventory has an invalid relative path",
        ));
    }
    let mut depth = 0usize;
    for component in value.split('/') {
        if component.is_empty() || matches!(component, "." | "..") || component.len() > 255 {
            return Err(invalid(
                "native artifact inventory has an invalid path component",
            ));
        }
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(invalid(
                "native artifact inventory exceeds its path depth limit",
            ));
        }
    }
    Ok(())
}

fn absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "native artifact publication cannot replace an existing destination",
        )),
    }
}

fn parent(path: &Path) -> io::Result<&Path> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid(
            "native artifact publication requires absolute paths",
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("native artifact publication has no directory name"))?;
    relative(name)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("native artifact publication has no parent directory"))?;
    physical_directory(parent)?;
    Ok(parent)
}

struct Inventory {
    files: BTreeSet<String>,
    destinations: BTreeSet<String>,
    entries: usize,
    bytes: u64,
    fingerprints: Vec<FileFingerprint>,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct FileFingerprint {
    path: String,
    length: u64,
    sha256: String,
}

impl FileFingerprint {
    pub(super) fn path(&self) -> &str {
        &self.path
    }
}

pub(super) fn fingerprints(root: &Path) -> io::Result<Vec<FileFingerprint>> {
    let mut files = Inventory::read(root)?.fingerprints;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

pub(super) fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid("native artifact requires a bounded regular file"));
    }
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != metadata.len() {
        return Err(invalid("native artifact file changed before reading"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(invalid("native artifact file changed while reading"));
    }
    Ok(bytes)
}

impl Inventory {
    fn read(root: &Path) -> io::Result<Self> {
        physical_directory(root)?;
        let mut inventory = Self {
            files: BTreeSet::new(),
            destinations: BTreeSet::new(),
            entries: 0,
            bytes: 0,
            fingerprints: Vec::new(),
        };
        inventory.walk(root, "")?;
        if inventory.files.is_empty() {
            return Err(invalid(
                "native artifact inventory contains no regular files",
            ));
        }
        Ok(inventory)
    }

    fn file(&mut self, path: &Path, name: String, metadata: &fs::Metadata) -> io::Result<()> {
        if self.files.len() >= MAX_FILES || metadata.len() > MAX_FILE_BYTES {
            return Err(invalid(
                "native artifact exceeds its file count or size limit",
            ));
        }
        self.bytes = self
            .bytes
            .checked_add(metadata.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(|| invalid("native artifact exceeds its total byte limit"))?;
        let mut file = fs::File::open(path)?;
        let opened = file.metadata()?;
        if !opened.is_file()
            || opened.len() != metadata.len()
            || opened.modified()? != metadata.modified()?
        {
            return Err(invalid("native artifact file changed before reading"));
        }
        let mut bytes = 0u64;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .filter(|total| *total <= opened.len())
                .ok_or_else(|| invalid("native artifact file grew while reading"))?;
            digest.update(&buffer[..count]);
        }
        let completed = file.metadata()?;
        if bytes != opened.len()
            || completed.len() != opened.len()
            || completed.modified()? != opened.modified()?
        {
            return Err(invalid("native artifact file changed while reading"));
        }
        self.fingerprints.push(FileFingerprint {
            path: name.clone(),
            length: bytes,
            sha256: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        });
        self.files.insert(name);
        Ok(())
    }

    fn walk(&mut self, root: &Path, prefix: &str) -> io::Result<()> {
        if !fs::symlink_metadata(root)?.is_dir() {
            return Err(invalid(
                "native artifact inventory contains a directory link",
            ));
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            self.entries = self
                .entries
                .checked_add(1)
                .filter(|total| *total <= MAX_ENTRIES)
                .ok_or_else(|| invalid("native artifact exceeds its entry limit"))?;
            let entry_name = entry.file_name();
            let entry_name = entry_name
                .to_str()
                .ok_or_else(|| invalid("native artifact entry name is not UTF-8"))?;
            let name = if prefix.is_empty() {
                entry_name.to_owned()
            } else {
                format!("{prefix}/{entry_name}")
            };
            relative(&name)?;
            if !self.destinations.insert(name.to_ascii_lowercase()) {
                return Err(invalid(
                    "native artifact inventory has duplicate destinations",
                ));
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                self.walk(&path, &name)?;
            } else if metadata.is_file() {
                self.file(&path, name, &metadata)?;
            } else {
                return Err(invalid(
                    "native artifact inventory permits only regular files",
                ));
            }
        }
        Ok(())
    }
}

pub(super) struct NativeArtifactPublication {
    staging: PathBuf,
    destination: PathBuf,
}

impl NativeArtifactPublication {
    pub(super) fn create(staging: &Path, destination: &Path) -> io::Result<Self> {
        if parent(staging)? != parent(destination)? || staging == destination {
            return Err(invalid(
                "native artifact publication requires distinct sibling paths",
            ));
        }
        absent(destination)?;
        fs::create_dir(staging)?;
        Ok(Self {
            staging: staging.to_owned(),
            destination: destination.to_owned(),
        })
    }

    pub(super) fn staging(&self) -> &Path {
        &self.staging
    }

    pub(super) fn publish_completed<T>(
        self,
        completed: io::Result<T>,
    ) -> io::Result<(PublishedNativeArtifact, T)> {
        let completed = completed?;
        self.publish().map(|artifact| (artifact, completed))
    }

    fn publish(self) -> io::Result<PublishedNativeArtifact> {
        Inventory::read(&self.staging)?;
        parent(&self.destination)?;
        absent(&self.destination)?;
        fs::rename(&self.staging, &self.destination)?;
        Ok(PublishedNativeArtifact {
            directory: self.destination,
        })
    }
}

pub(super) struct PublishedNativeArtifact {
    directory: PathBuf,
}

impl PublishedNativeArtifact {
    pub(super) fn read(directory: &Path, expected: &[&str]) -> io::Result<Self> {
        if expected.is_empty() || expected.len() > MAX_FILES {
            return Err(invalid(
                "native artifact expected inventory has an invalid file count",
            ));
        }
        let mut files = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        for name in expected {
            relative(name)?;
            if !destinations.insert(name.to_ascii_lowercase()) {
                return Err(invalid(
                    "native artifact expected inventory has duplicate paths",
                ));
            }
            files.insert((*name).to_owned());
        }
        if Inventory::read(directory)?.files != files {
            return Err(invalid(
                "native artifact differs from its expected file inventory",
            ));
        }
        Ok(Self {
            directory: directory.to_owned(),
        })
    }

    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
}
