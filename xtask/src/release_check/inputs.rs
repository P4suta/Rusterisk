use sha2::{Digest, Sha256};
use std::{
    fmt,
    path::{Component, Path},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Empty,
    Path,
    Duplicate,
    Limit,
    Missing,
    Changed,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "release input sets must be nonempty",
            Self::Path => "release inputs require bounded relative paths",
            Self::Duplicate => "release inputs contain duplicate paths",
            Self::Limit => "release inputs exceed their file or byte limit",
            Self::Missing => "runner input is missing from the release source",
            Self::Changed => "runner input differs from the release source",
        })
    }
}

impl std::error::Error for InputError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputKey([u8; 32]);

struct File<'a> {
    path: String,
    bytes: &'a [u8],
}

pub struct SourceInputs<'a> {
    files: Vec<File<'a>>,
    key: InputKey,
}

fn relative(path: &Path) -> Result<String, InputError> {
    let raw = path.to_str().ok_or(InputError::Path)?;
    if raw.is_empty()
        || raw.len() > 4096
        || matches!(raw.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        || raw
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
    {
        return Err(InputError::Path);
    }
    let mut portable = String::new();
    let mut depth = 0;
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(InputError::Path);
        };
        let value = value.to_str().ok_or(InputError::Path)?;
        if value
            .chars()
            .any(|character| character.is_control() || character == '\\')
        {
            return Err(InputError::Path);
        }
        depth += 1;
        if depth > 16 {
            return Err(InputError::Path);
        }
        if !portable.is_empty() {
            portable.push('/');
        }
        portable.push_str(value);
    }
    if portable.is_empty() {
        return Err(InputError::Path);
    }
    Ok(portable)
}

impl<'a> SourceInputs<'a> {
    pub const MAX_FILES: usize = crate::browser::SourceSnapshot::MAX_FILES;
    pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
    pub const MAX_TOTAL_BYTES: usize = 40 * 1024 * 1024;

    pub fn new<'p>(inputs: impl Iterator<Item = (&'p Path, &'a [u8])>) -> Result<Self, InputError> {
        let mut files = Vec::new();
        let mut total = 0usize;
        for (path, bytes) in inputs {
            if files.len() >= Self::MAX_FILES || bytes.len() > Self::MAX_FILE_BYTES {
                return Err(InputError::Limit);
            }
            total = total
                .checked_add(bytes.len())
                .filter(|total| *total <= Self::MAX_TOTAL_BYTES)
                .ok_or(InputError::Limit)?;
            let path = relative(path)?;
            if files.iter().any(|file: &File<'_>| file.path == path) {
                return Err(InputError::Duplicate);
            }
            files.push(File { path, bytes });
        }
        if files.is_empty() {
            return Err(InputError::Empty);
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut digest = Sha256::new();
        digest.update(b"rusterisk-release-inputs\0");
        for file in &files {
            digest.update((file.path.len() as u64).to_le_bytes());
            digest.update(file.path.as_bytes());
            digest.update((file.bytes.len() as u64).to_le_bytes());
            digest.update(file.bytes);
        }
        Ok(Self {
            files,
            key: InputKey(digest.finalize().into()),
        })
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub const fn key(&self) -> InputKey {
        self.key
    }

    pub fn bind(&self, runner: &Self) -> Result<InputKey, InputError> {
        for expected in &runner.files {
            let actual = self
                .files
                .binary_search_by(|file| file.path.cmp(&expected.path))
                .map_err(|_| InputError::Missing)?;
            if self.files[actual].bytes != expected.bytes {
                return Err(InputError::Changed);
            }
        }
        Ok(runner.key)
    }
}
