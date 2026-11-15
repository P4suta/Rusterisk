use super::{normalize_manifest, validate_locks, FuzzError};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
mod tests;

#[derive(PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

pub struct Snapshot {
    pub files: Vec<File>,
    pub normalized: String,
}

#[derive(Clone, Copy)]
enum FileBounds {
    Project,
    Vendor,
}

fn directory(path: &Path) -> Result<(), FuzzError> {
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(FuzzError::Manifest);
    }
    Ok(())
}

fn parents(root: &Path, path: &Path) -> Result<(), FuzzError> {
    let relative = path.strip_prefix(root).map_err(|_| FuzzError::Manifest)?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| match component {
            Component::Normal(name) => name.to_str().is_none_or(|name| {
                name.chars()
                    .any(|value| value.is_control() || value == '\\')
            }),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => true,
        })
    {
        return Err(FuzzError::Manifest);
    }
    let mut ancestor = root.to_owned();
    for component in relative.parent().ok_or(FuzzError::Manifest)?.components() {
        ancestor.push(component);
        directory(&ancestor)?;
    }
    Ok(())
}

pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, FuzzError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() || kind.is_symlink() {
        return Err(FuzzError::Manifest);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(FuzzError::Manifest);
    }
    Ok(bytes)
}

fn add(root: &Path, path: &Path, files: &mut Vec<File>) -> Result<(), FuzzError> {
    add_file(root, path, files, FileBounds::Project)
}

fn add_file(
    root: &Path,
    path: &Path,
    files: &mut Vec<File>,
    bounds: FileBounds,
) -> Result<(), FuzzError> {
    parents(root, path)?;
    if files.len() >= 1024 {
        return Err(FuzzError::Manifest);
    }
    let limit = match bounds {
        FileBounds::Project => 1024 * 1024,
        FileBounds::Vendor => 8 * 1024 * 1024,
    };
    let bytes = read_bounded(path, limit)?;
    let total: usize = files.iter().map(|file| file.bytes.len()).sum();
    let vendor: usize = files
        .iter()
        .filter(|file| file.path.starts_with("vendor/str0m"))
        .map(|file| file.bytes.len())
        .sum();
    if total + bytes.len() > 40 * 1024 * 1024
        || (matches!(bounds, FileBounds::Vendor) && vendor + bytes.len() > 12 * 1024 * 1024)
    {
        return Err(FuzzError::Manifest);
    }
    files.push(File {
        path: path
            .strip_prefix(root)
            .map_err(|_| FuzzError::Manifest)?
            .into(),
        bytes,
    });
    Ok(())
}

fn walk(root: &Path, directory: &Path, files: &mut Vec<File>, depth: u8) -> Result<(), FuzzError> {
    walk_files(root, directory, files, depth, FileBounds::Project)
}

fn walk_files(
    root: &Path,
    path: &Path,
    files: &mut Vec<File>,
    depth: u8,
    bounds: FileBounds,
) -> Result<(), FuzzError> {
    parents(root, path)?;
    directory(path)?;
    if depth > 8 {
        return Err(FuzzError::Manifest);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(FuzzError::Manifest);
        }
        if kind.is_dir() {
            walk_files(root, &entry.path(), files, depth + 1, bounds)?;
        } else if kind.is_file() {
            add_file(root, &entry.path(), files, bounds)?;
        } else {
            return Err(FuzzError::Manifest);
        }
    }
    Ok(())
}

fn vendor(root: &Path, manifest: &toml::Value, files: &mut Vec<File>) -> Result<(), FuzzError> {
    let path = manifest
        .get("patch")
        .and_then(|value| value.get("crates-io"))
        .and_then(|value| value.get("str0m"))
        .and_then(|value| value.get("path"));
    let Some(path) = path else { return Ok(()) };
    if path.as_str() != Some("vendor/str0m") {
        return Err(FuzzError::Manifest);
    }
    for path in ["vendor/str0m-source.toml", "vendor/str0m.patch"] {
        add(root, &root.join(path), files)?;
    }
    let before = files.len();
    walk_files(
        root,
        &root.join("vendor/str0m"),
        files,
        0,
        FileBounds::Vendor,
    )?;
    if files.len() == before {
        return Err(FuzzError::Manifest);
    }
    Ok(())
}

impl Snapshot {
    pub fn read(root: &Path) -> Result<Self, FuzzError> {
        directory(root)?;
        let mut files = Vec::new();
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "xtask/fuzz/Cargo.toml",
            "xtask/fuzz/Cargo.lock",
        ] {
            add(root, &root.join(path), &mut files)?;
        }
        let manifest = std::str::from_utf8(&files[0].bytes).map_err(|_| FuzzError::Manifest)?;
        let manifest: toml::Value = toml::from_str(manifest).map_err(FuzzError::Toml)?;
        let members = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("members"))
            .and_then(toml::Value::as_array)
            .ok_or(FuzzError::Manifest)?;
        if members.is_empty() || members.len() > 32 {
            return Err(FuzzError::Manifest);
        }
        for member in members {
            let member = Path::new(member.as_str().ok_or(FuzzError::Manifest)?);
            if member
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(FuzzError::Manifest);
            }
            let package = root.join(member);
            add(root, &package.join("Cargo.toml"), &mut files)?;
            for path in ["src", "tests"] {
                let directory = package.join(path);
                if directory.try_exists()? {
                    walk(root, &directory, &mut files, 0)?;
                }
            }
            let build = package.join("build.rs");
            if build.try_exists()? {
                add(root, &build, &mut files)?;
            }
        }
        for path in ["src", "tests", "fuzz_targets"] {
            walk(root, &root.join("xtask/fuzz").join(path), &mut files, 0)?;
        }
        vendor(root, &manifest, &mut files)?;
        files.sort_by(|left, right| left.path.cmp(&right.path));
        if files.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(FuzzError::Manifest);
        }
        let text = |path: &str| -> Result<&str, FuzzError> {
            let file = files
                .iter()
                .find(|file| file.path == Path::new(path))
                .ok_or(FuzzError::Manifest)?;
            std::str::from_utf8(&file.bytes).map_err(|_| FuzzError::Manifest)
        };
        let normalized = normalize_manifest(text("xtask/fuzz/Cargo.toml")?)?;
        validate_locks(text("Cargo.lock")?, text("xtask/fuzz/Cargo.lock")?)?;
        Ok(Self { files, normalized })
    }

    pub fn write(&self, source: &Path, state: &Path) -> Result<(), FuzzError> {
        for file in &self.files {
            write_new(&source.join(&file.path), &file.bytes)?;
            if let Ok(path) = file.path.strip_prefix("xtask/fuzz") {
                if path != Path::new("Cargo.toml") {
                    write_new(&state.join("fuzz").join(path), &file.bytes)?;
                }
            }
        }
        write_new(&state.join("fuzz/Cargo.toml"), self.normalized.as_bytes())
    }

    pub fn verify_source(&self, source: &Path) -> Result<(), FuzzError> {
        if Self::read(source)?.files != self.files {
            return Err(FuzzError::SourceChanged);
        }
        Ok(())
    }

    pub fn check_harness(&self, state: &Path) -> Result<(), FuzzError> {
        if read_bounded(&state.join("fuzz/Cargo.toml"), 1024 * 1024)? != self.normalized.as_bytes()
        {
            return Err(FuzzError::Manifest);
        }
        for file in &self.files {
            if let Ok(path) = file.path.strip_prefix("xtask/fuzz") {
                if path != Path::new("Cargo.toml")
                    && read_bounded(&state.join("fuzz").join(path), 1024 * 1024)? != file.bytes
                {
                    return Err(FuzzError::Manifest);
                }
            }
        }
        Ok(())
    }
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), FuzzError> {
    std::fs::create_dir_all(path.parent().ok_or(FuzzError::Plan)?)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, bytes)?;
    Ok(())
}
