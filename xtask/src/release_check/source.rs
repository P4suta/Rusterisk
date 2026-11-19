use super::{InputError, SourceInputs};
use crate::distribution::{SnapshotError, SourceSnapshot};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const FILES: [&str; 8] = [
    "rust-toolchain.toml",
    "clippy.toml",
    "deny.toml",
    "justfile",
    ".gitignore",
    "crates/core/proofs/Cargo.lock",
    "crates/core/assurance/Cargo.lock",
    "docs/server-distribution.md",
];
const DIRECTORIES: [&str; 6] = [
    "api",
    "containers",
    ".github/workflows",
    "tests/interop/node-ari-client",
    "tests/interop/sipp",
    "docs/adr",
];

#[derive(Debug)]
pub enum ReleaseSourceError {
    Distribution(SnapshotError),
    Io(io::Error),
    Inputs(InputError),
    Invalid,
    Changed,
}

impl std::fmt::Display for ReleaseSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Distribution(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::Inputs(error) => error.fmt(formatter),
            Self::Invalid => {
                formatter.write_str("release verification inputs require bounded regular files")
            }
            Self::Changed => formatter.write_str("captured release verification inputs changed"),
        }
    }
}
impl std::error::Error for ReleaseSourceError {}
impl From<io::Error> for ReleaseSourceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<SnapshotError> for ReleaseSourceError {
    fn from(error: SnapshotError) -> Self {
        Self::Distribution(error)
    }
}
impl From<InputError> for ReleaseSourceError {
    fn from(error: InputError) -> Self {
        Self::Inputs(error)
    }
}

struct File {
    path: PathBuf,
    bytes: Vec<u8>,
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;

struct Remaining<'a> {
    files: usize,
    bytes: usize,
    distribution: &'a SourceSnapshot,
}

fn directory(path: &Path) -> Result<(), ReleaseSourceError> {
    if fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(ReleaseSourceError::Invalid)
    }
}

fn parents(root: &Path, relative: &Path) -> Result<(), ReleaseSourceError> {
    let mut path = root.to_owned();
    for component in relative
        .parent()
        .ok_or(ReleaseSourceError::Invalid)?
        .components()
    {
        path.push(component);
        directory(&path)?;
    }
    Ok(())
}

fn add(
    root: &Path,
    relative: &Path,
    files: &mut Vec<File>,
    remaining: &mut Remaining<'_>,
) -> Result<(), ReleaseSourceError> {
    let shared = remaining
        .distribution
        .files()
        .find_map(|(path, bytes)| (path == relative).then_some(bytes));
    if shared.is_none() && remaining.files == 0 {
        return Err(InputError::Limit.into());
    }
    parents(root, relative)?;
    let path = root.join(relative);
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err(ReleaseSourceError::Invalid);
    }
    let limit = if shared.is_some() {
        1024 * 1024
    } else {
        remaining.bytes.min(1024 * 1024)
    };
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(InputError::Limit.into());
    }
    if let Some(expected) = shared {
        return if bytes == expected {
            Ok(())
        } else {
            Err(ReleaseSourceError::Changed)
        };
    }
    remaining.files -= 1;
    remaining.bytes -= bytes.len();
    files.push(File {
        path: relative.into(),
        bytes,
    });
    Ok(())
}

fn walk(
    root: &Path,
    relative: &Path,
    files: &mut Vec<File>,
    remaining: &mut Remaining<'_>,
    depth: u8,
) -> Result<(), ReleaseSourceError> {
    if depth > 8 {
        return Err(InputError::Limit.into());
    }
    parents(root, relative)?;
    directory(&root.join(relative))?;
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        if depth == 0
            && relative == Path::new("tests/interop/node-ari-client")
            && entry.file_name() == "node_modules"
        {
            continue;
        }
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &path, files, remaining, depth + 1)?;
        } else {
            add(root, &path, files, remaining)?;
        }
    }
    Ok(())
}

pub struct CapturedReleaseSource {
    distribution: SourceSnapshot,
    extra: Vec<File>,
}

impl CapturedReleaseSource {
    pub fn read(root: &Path) -> Result<Self, ReleaseSourceError> {
        let distribution = SourceSnapshot::read(root)?;
        let total = distribution.files().try_fold(0usize, |total, (_, bytes)| {
            total.checked_add(bytes.len()).ok_or(InputError::Limit)
        })?;
        let mut remaining = Remaining {
            distribution: &distribution,
            files: SourceInputs::MAX_FILES
                .checked_sub(distribution.files().count())
                .ok_or(InputError::Limit)?,
            bytes: SourceInputs::MAX_TOTAL_BYTES
                .checked_sub(total)
                .ok_or(InputError::Limit)?,
        };
        let mut extra = Vec::new();
        for path in FILES {
            add(root, Path::new(path), &mut extra, &mut remaining)?;
        }
        for path in DIRECTORIES {
            walk(root, Path::new(path), &mut extra, &mut remaining, 0)?;
        }
        extra.sort_by(|left, right| left.path.cmp(&right.path));
        let captured = Self {
            distribution,
            extra,
        };
        captured.inputs()?;
        Ok(captured)
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.distribution.files().chain(
            self.extra
                .iter()
                .map(|file| (file.path.as_path(), file.bytes.as_slice())),
        )
    }

    pub fn inputs(&self) -> Result<SourceInputs<'_>, InputError> {
        SourceInputs::new(self.files())
    }

    pub fn write(&self, source: &Path, state: &Path) -> Result<(), ReleaseSourceError> {
        self.distribution.write(source, state)?;
        for directory in DIRECTORIES {
            fs::create_dir_all(source.join(directory))?;
        }
        for file in &self.extra {
            let path = source.join(&file.path);
            fs::create_dir_all(path.parent().ok_or(ReleaseSourceError::Invalid)?)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .write_all(&file.bytes)?;
        }
        Ok(())
    }

    pub fn check(&self, source: &Path) -> Result<(), ReleaseSourceError> {
        let captured = Self::read(source)?;
        if self.files().eq(captured.files()) {
            Ok(())
        } else {
            Err(ReleaseSourceError::Changed)
        }
    }
}
