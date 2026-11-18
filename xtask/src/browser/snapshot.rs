use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const MAX_SOURCE_FILES: usize = 2048;
const SHARED_ADAPTER: &str = "tests/interop/node-ari-client/project-service-only";
const SHARED_ADAPTER_FILES: [&str; 3] = ["package.json", "index.mjs", "index.ts"];

const BROWSER_FILES: [&str; 13] = [
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "eslint.config.mjs",
    "biome.json",
    "playwright.config.ts",
    "index.html",
    "style.css",
    "config/http.conf",
    "config/ari.conf",
    "config/pjsip.conf",
    "config/rtp.conf",
    "config/extensions.conf",
];

#[derive(Debug)]
pub enum SourceError {
    Io(io::Error),
    Invalid,
    Changed,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Invalid => formatter
                .write_str("browser source snapshot requires bounded regular project files"),
            Self::Changed => {
                formatter.write_str("browser source or owned fixture changed during verification")
            }
        }
    }
}
impl std::error::Error for SourceError {}
impl From<io::Error> for SourceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(PartialEq, Eq)]
struct File {
    path: PathBuf,
    bytes: Vec<u8>,
}

pub struct SourceSnapshot {
    files: Vec<File>,
}

#[derive(Clone, Copy)]
enum FileBounds {
    Project,
    Vendor,
    Archive,
}

fn directory(path: &Path) -> Result<(), SourceError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_dir() || kind.is_symlink() {
        return Err(SourceError::Invalid);
    }
    Ok(())
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, SourceError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() || kind.is_symlink() {
        return Err(SourceError::Invalid);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(SourceError::Invalid);
    }
    Ok(bytes)
}

fn normal(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| match component {
            Component::Normal(value) => value.to_str().is_some_and(|value| {
                !value
                    .chars()
                    .any(|character| character.is_control() || character == '\\')
            }),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => false,
        })
}

fn parents(root: &Path, relative: &Path) -> Result<(), SourceError> {
    if !normal(relative) {
        return Err(SourceError::Invalid);
    }
    let mut path = root.to_owned();
    for component in relative.parent().ok_or(SourceError::Invalid)?.components() {
        path.push(component);
        directory(&path)?;
    }
    Ok(())
}

fn add(path: &Path, relative: &Path, files: &mut Vec<File>) -> Result<(), SourceError> {
    add_file(path, relative, files, FileBounds::Project)
}

fn add_file(
    path: &Path,
    relative: &Path,
    files: &mut Vec<File>,
    bounds: FileBounds,
) -> Result<(), SourceError> {
    if !normal(relative) || files.len() >= MAX_SOURCE_FILES {
        return Err(SourceError::Invalid);
    }
    let limit = match bounds {
        FileBounds::Project => 1024 * 1024,
        FileBounds::Vendor | FileBounds::Archive => 8 * 1024 * 1024,
    };
    let bytes = read(path, limit)?;
    let total: usize = files.iter().map(|file| file.bytes.len()).sum();
    let vendor: usize = files
        .iter()
        .filter(|file| file.path.starts_with("vendor/str0m"))
        .map(|file| file.bytes.len())
        .sum();
    if total + bytes.len() > 40 * 1024 * 1024
        || (matches!(bounds, FileBounds::Vendor) && vendor + bytes.len() > 12 * 1024 * 1024)
    {
        return Err(SourceError::Invalid);
    }
    files.push(File {
        path: relative.into(),
        bytes,
    });
    Ok(())
}

fn walk(path: &Path, relative: &Path, files: &mut Vec<File>, depth: u8) -> Result<(), SourceError> {
    walk_files(path, relative, files, depth, FileBounds::Project)
}

fn walk_files(
    path: &Path,
    relative: &Path,
    files: &mut Vec<File>,
    depth: u8,
    bounds: FileBounds,
) -> Result<(), SourceError> {
    directory(path)?;
    if depth > 8 {
        return Err(SourceError::Invalid);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(SourceError::Invalid);
        }
        if kind.is_dir() {
            walk_files(&entry.path(), &relative, files, depth + 1, bounds)?;
        } else {
            add_file(&entry.path(), &relative, files, bounds)?;
        }
    }
    Ok(())
}

fn vendor(root: &Path, manifest: &toml::Value, files: &mut Vec<File>) -> Result<(), SourceError> {
    let path = manifest
        .get("patch")
        .and_then(|value| value.get("crates-io"))
        .and_then(|value| value.get("str0m"))
        .and_then(|value| value.get("path"));
    let Some(path) = path else { return Ok(()) };
    if path.as_str() != Some("vendor/str0m") {
        return Err(SourceError::Invalid);
    }
    let archive = Path::new("vendor/archives/str0m-0.25.0.crate");
    parents(root, archive)?;
    add_file(&root.join(archive), archive, files, FileBounds::Archive)?;
    for path in ["vendor/str0m-source.toml", "vendor/str0m.patch"] {
        parents(root, Path::new(path))?;
        add(&root.join(path), Path::new(path), files)?;
    }
    parents(root, Path::new("vendor/str0m"))?;
    let before = files.len();
    walk_files(
        &root.join("vendor/str0m"),
        Path::new("vendor/str0m"),
        files,
        0,
        FileBounds::Vendor,
    )?;
    if files.len() == before {
        return Err(SourceError::Invalid);
    }
    Ok(())
}

fn optional_walk(root: &Path, relative: &Path, files: &mut Vec<File>) -> Result<(), SourceError> {
    if let Err(error) = parents(root, relative) {
        return match error {
            SourceError::Io(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            error @ (SourceError::Io(_) | SourceError::Invalid | SourceError::Changed) => {
                Err(error)
            }
        };
    }
    match std::fs::symlink_metadata(root.join(relative)) {
        Ok(_) => walk(&root.join(relative), relative, files, 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SourceError::Io(error)),
    }
}

fn browser(path: &Path, files: &mut Vec<File>) -> Result<(), SourceError> {
    directory(path)?;
    for file in BROWSER_FILES {
        parents(path, Path::new(file))?;
        add(
            &path.join(file),
            &Path::new("examples/browser").join(file),
            files,
        )?;
    }
    for name in ["src", "tests"] {
        let before = files.len();
        walk(
            &path.join(name),
            &Path::new("examples/browser").join(name),
            files,
            0,
        )?;
        if files.len() == before {
            return Err(SourceError::Invalid);
        }
    }
    Ok(())
}

fn shared_adapter(root: &Path, files: &mut Vec<File>) -> Result<(), SourceError> {
    let relative = Path::new(SHARED_ADAPTER);
    parents(root, relative)?;
    let path = root.join(relative);
    directory(&path)?;
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| SHARED_ADAPTER_FILES.contains(&name))
            || !entry.file_type()?.is_file()
        {
            return Err(SourceError::Invalid);
        }
    }
    for name in SHARED_ADAPTER_FILES {
        let relative = relative.join(name);
        parents(root, &relative)?;
        add(&root.join(&relative), &relative, files)?;
    }
    Ok(())
}

impl SourceSnapshot {
    pub const MAX_FILES: usize = MAX_SOURCE_FILES;

    pub fn files(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.files
            .iter()
            .map(|file| (file.path.as_path(), file.bytes.as_slice()))
    }

    pub fn read(root: &Path) -> Result<Self, SourceError> {
        directory(root)?;
        let mut files = Vec::new();
        for path in ["Cargo.toml", "Cargo.lock"] {
            add(&root.join(path), Path::new(path), &mut files)?;
        }
        let manifest = std::str::from_utf8(&files[0].bytes).map_err(|_| SourceError::Invalid)?;
        let manifest: toml::Value = toml::from_str(manifest).map_err(|_| SourceError::Invalid)?;
        let members = manifest
            .get("workspace")
            .and_then(|value| value.get("members"))
            .and_then(toml::Value::as_array)
            .ok_or(SourceError::Invalid)?;
        if members.is_empty() || members.len() > 32 {
            return Err(SourceError::Invalid);
        }
        for member in members {
            let member = Path::new(member.as_str().ok_or(SourceError::Invalid)?);
            if !normal(member) {
                return Err(SourceError::Invalid);
            }
            let mut directory_path = root.to_owned();
            for component in member.components() {
                directory_path.push(component);
                directory(&directory_path)?;
            }
            add(
                &root.join(member).join("Cargo.toml"),
                &member.join("Cargo.toml"),
                &mut files,
            )?;
            for name in ["src", "tests", "examples"] {
                optional_walk(root, &member.join(name), &mut files)?;
            }
            match std::fs::symlink_metadata(root.join(member).join("build.rs")) {
                Ok(_) => add(
                    &root.join(member).join("build.rs"),
                    &member.join("build.rs"),
                    &mut files,
                )?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(SourceError::Io(error)),
            }
        }
        for path in ["xtask/fuzz/Cargo.toml", "xtask/fuzz/Cargo.lock"] {
            parents(root, Path::new(path))?;
            add(&root.join(path), Path::new(path), &mut files)?;
        }
        for path in [
            "xtask/fuzz/src",
            "xtask/fuzz/tests",
            "xtask/fuzz/fuzz_targets",
            "tests/interop/asterisk",
            "tests/interop/asterisk-browser",
        ] {
            optional_walk(root, Path::new(path), &mut files)?;
        }
        parents(root, Path::new("examples/browser"))?;
        browser(&root.join("examples/browser"), &mut files)?;
        shared_adapter(root, &mut files)?;
        vendor(root, &manifest, &mut files)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if files.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(SourceError::Invalid);
        }
        Ok(Self { files })
    }

    pub fn digest(&self) -> String {
        let mut digest = Sha256::new();
        for file in &self.files {
            let path = file.path.to_string_lossy();
            digest.update((path.len() as u64).to_le_bytes());
            digest.update(path.as_bytes());
            digest.update((file.bytes.len() as u64).to_le_bytes());
            digest.update(&file.bytes);
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn write(&self, source: &Path, state: &Path) -> Result<(), SourceError> {
        std::fs::create_dir(source)?;
        std::fs::create_dir(state)?;
        for file in &self.files {
            let path = source.join(&file.path);
            write(&path, &file.bytes)?;
            if let Ok(relative) = file.path.strip_prefix("examples/browser") {
                write(&state.join("browser").join(relative), &file.bytes)?;
            }
        }
        Ok(())
    }

    pub fn check_original(&self, root: &Path) -> Result<(), SourceError> {
        if Self::read(root)?.files == self.files {
            Ok(())
        } else {
            Err(SourceError::Changed)
        }
    }

    pub fn check_browser(&self, state: &Path) -> Result<(), SourceError> {
        directory(state)?;
        let mut files = Vec::new();
        browser(&state.join("browser"), &mut files)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if files.iter().eq(self
            .files
            .iter()
            .filter(|file| file.path.starts_with("examples/browser")))
        {
            Ok(())
        } else {
            Err(SourceError::Changed)
        }
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), SourceError> {
    std::fs::create_dir_all(path.parent().ok_or(SourceError::Invalid)?)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}
