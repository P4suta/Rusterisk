use super::{validate_harnesses, validate_manifest, ProofError};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

#[derive(PartialEq, Eq)]
struct InputFile {
    path: PathBuf,
    bytes: Vec<u8>,
}

fn directory(path: &Path) -> Result<(), ProofError> {
    if fs::symlink_metadata(path)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(ProofError::Plan)
    }
}

fn bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ProofError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(ProofError::Plan);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ProofError::Plan);
    }
    Ok(bytes)
}

fn relative(root: &Path, path: &Path) -> Result<PathBuf, ProofError> {
    let path = path.strip_prefix(root).map_err(|_| ProofError::Plan)?;
    if !path.components().all(|component| match component {
        Component::Normal(value) => value.to_str().is_some_and(|value| {
            !value
                .chars()
                .any(|character| character.is_control() || character == '\\')
        }),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    }) {
        return Err(ProofError::Plan);
    }
    Ok(path.to_owned())
}

fn visit(
    root: &Path,
    path: &Path,
    depth: usize,
    files: &mut Vec<InputFile>,
    total: &mut usize,
) -> Result<(), ProofError> {
    if depth > 8 {
        return Err(ProofError::Plan);
    }
    directory(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            return Err(ProofError::Plan);
        }
        if kind.is_dir() {
            visit(root, &path, depth + 1, files, total)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            if files.len() >= 64 {
                return Err(ProofError::Plan);
            }
            let bytes = bounded(&path, 1024 * 1024)?;
            *total = total
                .checked_add(bytes.len())
                .filter(|total| *total <= 8 * 1024 * 1024)
                .ok_or(ProofError::Plan)?;
            files.push(InputFile {
                path: relative(root, &path)?,
                bytes,
            });
        }
    }
    Ok(())
}

pub struct CapturedCoreProofs {
    files: Vec<InputFile>,
}

impl CapturedCoreProofs {
    pub fn read(root: &Path) -> Result<Self, ProofError> {
        directory(root)?;
        for path in [
            "crates",
            "crates/core",
            "crates/core/src",
            "crates/core/proofs",
        ] {
            directory(&root.join(path))?;
        }
        let mut files = Vec::new();
        let mut total = 0usize;
        visit(
            root,
            &root.join("crates/core/src"),
            0,
            &mut files,
            &mut total,
        )?;
        for path in [
            "crates/core/proofs/Cargo.toml",
            "crates/core/proofs/Cargo.lock",
        ] {
            let bytes = bounded(&root.join(path), 16_384)?;
            total = total
                .checked_add(bytes.len())
                .filter(|total| *total <= 8 * 1024 * 1024)
                .ok_or(ProofError::Plan)?;
            files.push(InputFile {
                path: path.into(),
                bytes,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let text = |path: &str| {
            let bytes = files
                .iter()
                .find(|file| file.path == Path::new(path))
                .map(|file| file.bytes.as_slice())
                .ok_or(ProofError::Plan)?;
            std::str::from_utf8(bytes).map_err(|_| ProofError::Plan)
        };
        validate_manifest(text("crates/core/proofs/Cargo.toml")?)?;
        validate_harnesses(text("crates/core/src/proofs.rs")?)?;
        Ok(Self { files })
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.files
            .iter()
            .map(|file| (file.path.as_path(), file.bytes.as_slice()))
    }

    pub(super) fn write(&self, root: &Path) -> Result<(), ProofError> {
        for file in &self.files {
            let path = root.join(&file.path);
            fs::create_dir_all(path.parent().ok_or(ProofError::Plan)?)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?
                .write_all(&file.bytes)?;
        }
        Ok(())
    }

    pub(super) fn check(&self, root: &Path) -> Result<(), ProofError> {
        if Self::read(root)?.files == self.files {
            Ok(())
        } else {
            Err(ProofError::SourceChanged)
        }
    }
}
