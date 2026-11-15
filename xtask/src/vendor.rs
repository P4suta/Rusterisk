use crate::process::{run, ProcessError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::{Component, Path},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ARCHIVE_SHA256: &str = "357c7e453c033c8bb59e912312f9f9ff07c2a9ce7268bcb94f0c828a5b47e58f";
const FILE_LIMIT: usize = 8 * 1024 * 1024;
const TREE_LIMIT: usize = 12 * 1024 * 1024;

#[derive(Debug)]
pub enum VendorError {
    Io(io::Error),
    Decode(toml::de::Error),
    Encode(toml::ser::Error),
    Process(ProcessError),
    Invalid,
    Changed,
    Cleanup {
        operation: String,
        cleanup: io::Error,
    },
}
impl std::fmt::Display for VendorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Decode(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Invalid => formatter.write_str(
                "vendored sources require the pinned published identity and bounded regular files",
            ),
            Self::Changed => formatter.write_str(
                "reversing the vendor patch did not restore the complete published source",
            ),
            Self::Cleanup { operation, cleanup } => write!(
                formatter,
                "{operation}; vendor temporary cleanup failed: {cleanup}"
            ),
        }
    }
}
impl std::error::Error for VendorError {}
impl From<io::Error> for VendorError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<toml::de::Error> for VendorError {
    fn from(error: toml::de::Error) -> Self {
        Self::Decode(error)
    }
}
impl From<toml::ser::Error> for VendorError {
    fn from(error: toml::ser::Error) -> Self {
        Self::Encode(error)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    name: String,
    version: String,
    archive_sha256: String,
    files: BTreeMap<String, String>,
}

fn normal(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains(['\\', '\0', ':'])
        && path.split('/').all(|part| {
            !part.is_empty() && !matches!(part, "." | "..") && !part.eq_ignore_ascii_case(".git")
        })
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !path.chars().any(char::is_control)
}

fn hash_text(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Record {
    fn validate(&self) -> Result<(), VendorError> {
        if self.name != "str0m"
            || self.version != "0.25.0"
            || self.archive_sha256 != ARCHIVE_SHA256
            || self.files.is_empty()
            || self.files.len() > 320
            || self
                .files
                .iter()
                .any(|(path, hash)| !normal(path) || !hash_text(hash))
        {
            return Err(VendorError::Invalid);
        }
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, VendorError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > limit as u64 {
        return Err(VendorError::Invalid);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(VendorError::Invalid);
    }
    Ok(bytes)
}

fn directory(path: &Path) -> Result<(), VendorError> {
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(VendorError::Invalid);
    }
    Ok(())
}

fn walk(
    root: &Path,
    path: &Path,
    depth: usize,
    files: &mut BTreeMap<String, Vec<u8>>,
    total: &mut usize,
) -> Result<(), VendorError> {
    directory(path)?;
    if depth > 8 {
        return Err(VendorError::Invalid);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(VendorError::Invalid);
        }
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, depth + 1, files, total)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| VendorError::Invalid)?
                .to_str()
                .ok_or(VendorError::Invalid)?
                .replace('\\', "/");
            if !normal(&relative) || files.len() >= 320 {
                return Err(VendorError::Invalid);
            }
            let bytes = read(&path, FILE_LIMIT)?;
            *total += bytes.len();
            if *total > TREE_LIMIT {
                return Err(VendorError::Invalid);
            }
            if files.insert(relative, bytes).is_some() {
                return Err(VendorError::Invalid);
            }
        }
    }
    Ok(())
}

fn tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, VendorError> {
    let mut files = BTreeMap::new();
    walk(root, root, 0, &mut files, &mut 0)?;
    Ok(files)
}

fn write_tree(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), VendorError> {
    std::fs::create_dir(root)?;
    for (relative, bytes) in files {
        if !normal(relative) {
            return Err(VendorError::Invalid);
        }
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().ok_or(VendorError::Invalid)?)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(bytes)?;
    }
    Ok(())
}

fn copy_published(source: &Path, target: &Path) -> Result<(), VendorError> {
    let files = tree(source)?;
    write_tree(target, &files)?;
    for relative in files.keys() {
        let permissions = std::fs::symlink_metadata(source.join(relative))?.permissions();
        std::fs::set_permissions(target.join(relative), permissions)?;
    }
    Ok(())
}

fn temporary(
    operation: impl FnOnce(&Path) -> Result<usize, VendorError>,
) -> Result<usize, VendorError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("rusterisk-vendor-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&root)?;
    let operation = operation(&root);
    match (operation, std::fs::remove_dir_all(root)) {
        (Ok(count), Ok(())) => Ok(count),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(VendorError::Io(error)),
        (Err(operation), Err(cleanup)) => Err(VendorError::Cleanup {
            operation: operation.to_string(),
            cleanup,
        }),
    }
}

fn verify_tree(source: &Path, record: &Record, patch: &Path) -> Result<usize, VendorError> {
    record.validate()?;
    let files = tree(source)?;
    let patch_bytes = read(patch, 1024 * 1024)?;
    if patch_bytes.is_empty() {
        return Err(VendorError::Invalid);
    }
    temporary(|root| {
        let source = root.join("source");
        write_tree(&source, &files)?;
        let patch = root.join("source.patch");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&patch)?
            .write_all(&patch_bytes)?;
        run(
            Command::new("git")
                .args(["apply", "--reverse"])
                .arg(&patch)
                .current_dir(&source)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE"),
            Duration::from_secs(30),
        )
        .map_err(VendorError::Process)?;
        let restored: BTreeMap<_, _> = tree(&source)?
            .into_iter()
            .map(|(name, bytes)| (name, digest(&bytes)))
            .collect();
        if restored != record.files {
            return Err(VendorError::Changed);
        }
        Ok(restored.len())
    })
}

pub fn check(root: &Path) -> Result<usize, VendorError> {
    let manifest: toml::Value = toml::from_str(
        std::str::from_utf8(&read(&root.join("Cargo.toml"), 1024 * 1024)?)
            .map_err(|_| VendorError::Invalid)?,
    )?;
    let patch = manifest
        .get("patch")
        .and_then(|patch| patch.get("crates-io"))
        .and_then(|patch| patch.get("str0m"));
    let vendor = root.join("vendor");
    let Some(patch) = patch else {
        if vendor.join("str0m").exists() || vendor.join("str0m-source.toml").exists() {
            return Err(VendorError::Invalid);
        }
        return Ok(0);
    };
    if patch.get("path").and_then(toml::Value::as_str) != Some("vendor/str0m") {
        return Err(VendorError::Invalid);
    }
    directory(&vendor)?;
    let record: Record = toml::from_str(
        std::str::from_utf8(&read(&vendor.join("str0m-source.toml"), 128 * 1024)?)
            .map_err(|_| VendorError::Invalid)?,
    )?;
    verify_tree(&vendor.join("str0m"), &record, &vendor.join("str0m.patch"))
}

pub fn import(root: &Path, archive: &Path) -> Result<usize, VendorError> {
    let bytes = read(archive, FILE_LIMIT)?;
    if digest(&bytes) != ARCHIVE_SHA256 {
        return Err(VendorError::Invalid);
    }
    let vendor = root.join("vendor");
    match std::fs::create_dir(&vendor) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => directory(&vendor)?,
        Err(error) => return Err(error.into()),
    }
    if vendor.join("str0m").exists() || vendor.join("str0m-source.toml").exists() {
        return Err(VendorError::Invalid);
    }
    temporary(|temporary| {
        let archive = temporary.join("source.crate");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)?
            .write_all(&bytes)?;
        let source = temporary.join("source");
        std::fs::create_dir(&source)?;
        run(
            Command::new("tar")
                .args(["--extract", "--gzip", "--strip-components=1", "--file"])
                .arg(&archive)
                .arg("--directory")
                .arg(&source),
            Duration::from_secs(30),
        )
        .map_err(VendorError::Process)?;
        let files = tree(&source)?;
        let record = Record {
            name: "str0m".into(),
            version: "0.25.0".into(),
            archive_sha256: ARCHIVE_SHA256.into(),
            files: files
                .iter()
                .map(|(name, bytes)| (name.clone(), digest(bytes)))
                .collect(),
        };
        record.validate()?;
        let record = toml::to_string(&record)?;
        copy_published(&source, &vendor.join("str0m"))?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(vendor.join("str0m-source.toml"))?
            .write_all(record.as_bytes())?;
        Ok(files.len())
    })
}

pub fn patch(root: &Path, archive: &Path) -> Result<usize, VendorError> {
    let bytes = read(archive, FILE_LIMIT)?;
    if digest(&bytes) != ARCHIVE_SHA256 {
        return Err(VendorError::Invalid);
    }
    let vendor = root.join("vendor");
    directory(&vendor)?;
    let record: Record = toml::from_str(
        std::str::from_utf8(&read(&vendor.join("str0m-source.toml"), 128 * 1024)?)
            .map_err(|_| VendorError::Invalid)?,
    )?;
    record.validate()?;
    temporary(|temporary| {
        let archive = temporary.join("source.crate");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)?
            .write_all(&bytes)?;
        let original = temporary.join("a");
        std::fs::create_dir(&original)?;
        run(
            Command::new("tar")
                .args(["--extract", "--gzip", "--strip-components=1", "--file"])
                .arg(&archive)
                .arg("--directory")
                .arg(&original),
            Duration::from_secs(30),
        )
        .map_err(VendorError::Process)?;
        let original_hashes: BTreeMap<_, _> = tree(&original)?
            .into_iter()
            .map(|(name, bytes)| (name, digest(&bytes)))
            .collect();
        if original_hashes != record.files {
            return Err(VendorError::Changed);
        }
        write_tree(&temporary.join("b"), &tree(&vendor.join("str0m"))?)?;
        let output = temporary.join("source.patch");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        match run(
            Command::new("git")
                .args([
                    "diff",
                    "--no-index",
                    "--no-prefix",
                    "--binary",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--",
                    "a",
                    "b",
                ])
                .current_dir(temporary)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .stdout(file),
            Duration::from_secs(30),
        ) {
            Err(ProcessError::Status(status)) if status.code() == Some(1) => {}
            Err(error) => return Err(VendorError::Process(error)),
            Ok(()) => return Err(VendorError::Invalid),
        }
        let patch = read(&output, 1024 * 1024)?;
        let count = verify_tree(&vendor.join("str0m"), &record, &output)?;
        std::fs::write(vendor.join("str0m.patch"), patch)?;
        Ok(count)
    })
}

#[cfg(test)]
mod tests;
