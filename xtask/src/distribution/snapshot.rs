use super::{sdk, SdkInputError, SdkPackage};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

const EXTRA: [&str; 18] = [
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "docs/rust-sdk.md",
    "crates/core/LICENSE-MIT",
    "crates/core/LICENSE-APACHE",
    "crates/api/LICENSE-MIT",
    "crates/api/LICENSE-APACHE",
    "crates/client/LICENSE-MIT",
    "crates/client/LICENSE-APACHE",
    "crates/core/assurance/Cargo.toml",
    "crates/core/proofs/Cargo.toml",
    "mise.toml",
    ".cargo/config.toml",
    "config/http.conf",
    "config/ari.conf",
    "config/pjsip.conf",
    "config/rtp.conf",
    "config/extensions.conf",
];

#[derive(Debug)]
pub enum SnapshotError {
    Io(std::io::Error),
    Project(crate::browser::SourceError),
    Input(SdkInputError),
    Invalid,
    Limits { files: usize, bytes: usize },
    Changed,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::Input(error) => error.fmt(formatter),
            Self::Invalid => formatter.write_str(
                "distribution source requires bounded regular project and package inputs",
            ),
            Self::Changed => formatter.write_str("captured distribution source changed"),
            Self::Limits { files, bytes } => write!(
                formatter,
                "distribution source has {files} files and {bytes} bytes; limits are 1024 files and 41943040 bytes"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<std::io::Error> for SnapshotError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<crate::browser::SourceError> for SnapshotError {
    fn from(error: crate::browser::SourceError) -> Self {
        Self::Project(error)
    }
}
impl From<SdkInputError> for SnapshotError {
    fn from(error: SdkInputError) -> Self {
        Self::Input(error)
    }
}

struct File {
    path: PathBuf,
    bytes: Vec<u8>,
}

pub struct SourceSnapshot {
    project: crate::browser::SourceSnapshot,
    extra: Vec<File>,
}

fn package_roots(root: &Path) -> Result<(), SnapshotError> {
    for package in SdkPackage::ALL {
        for entry in std::fs::read_dir(root.join(package.directory()))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(SnapshotError::Invalid)?;
            if matches!(name, "target" | ".git") {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(SnapshotError::Invalid);
            }
            let allowed = if kind.is_file() {
                matches!(name, "Cargo.toml" | "LICENSE-MIT" | "LICENSE-APACHE")
            } else if kind.is_dir() {
                matches!(name, "src" | "tests" | "examples")
                    || (package == SdkPackage::Core && matches!(name, "assurance" | "proofs"))
            } else {
                false
            };
            if !allowed {
                return Err(SnapshotError::Invalid);
            }
        }
    }
    Ok(())
}

fn extra(root: &Path) -> Result<Vec<File>, SnapshotError> {
    let mut files = Vec::new();
    for path in EXTRA {
        let bytes = sdk::read(root, path)?;
        if bytes.len() > sdk::MAX_INPUT {
            return Err(SnapshotError::Invalid);
        }
        files.push(File {
            path: path.into(),
            bytes,
        });
    }
    package_roots(root)?;
    Ok(files)
}

impl SourceSnapshot {
    pub fn read(root: &Path) -> Result<Self, SnapshotError> {
        let project = crate::browser::SourceSnapshot::read(root)?;
        let extra = extra(root)?;
        let snapshot = Self { project, extra };
        let mut paths = BTreeSet::new();
        let mut total = 0usize;
        for (path, bytes) in snapshot.files() {
            if !paths.insert(path.to_owned()) {
                return Err(SnapshotError::Invalid);
            }
            total += bytes.len();
            if paths.len() > 1024 || total > 40 * 1024 * 1024 {
                return Err(SnapshotError::Limits {
                    files: paths.len(),
                    bytes: total,
                });
            }
        }
        Ok(snapshot)
    }

    pub fn write(&self, source: &Path, state: &Path) -> Result<(), SnapshotError> {
        self.project.write(source, state)?;
        for file in &self.extra {
            let path = source.join(&file.path);
            std::fs::create_dir_all(path.parent().ok_or(SnapshotError::Invalid)?)?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .write_all(&file.bytes)?;
        }
        Ok(())
    }

    pub fn check(&self, source: &Path) -> Result<(), SnapshotError> {
        self.project.check_original(source)?;
        let found = extra(source)?;
        if found
            .iter()
            .map(|file| (&file.path, &file.bytes))
            .eq(self.extra.iter().map(|file| (&file.path, &file.bytes)))
        {
            Ok(())
        } else {
            Err(SnapshotError::Changed)
        }
    }

    pub fn digest(&self) -> String {
        let mut files: Vec<_> = self
            .files()
            .map(|(path, bytes)| (path.to_string_lossy().replace('\\', "/"), bytes))
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let mut digest = Sha256::new();
        for (path, bytes) in files {
            digest.update((path.len() as u64).to_le_bytes());
            digest.update(path.as_bytes());
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.project.files().chain(
            self.extra
                .iter()
                .map(|file| (file.path.as_path(), file.bytes.as_slice())),
        )
    }
}
