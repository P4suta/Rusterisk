use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

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

fn directory(path: &Path) -> Result<(), SourceError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_dir() || kind.is_symlink() {
        return Err(SourceError::Invalid);
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, SourceError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() || kind.is_symlink() {
        return Err(SourceError::Invalid);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
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
    if !normal(relative) || files.len() >= 1024 {
        return Err(SourceError::Invalid);
    }
    let bytes = read(path)?;
    if files.iter().map(|file| file.bytes.len()).sum::<usize>() + bytes.len() > 24 * 1024 * 1024 {
        return Err(SourceError::Invalid);
    }
    files.push(File {
        path: relative.into(),
        bytes,
    });
    Ok(())
}

fn walk(path: &Path, relative: &Path, files: &mut Vec<File>, depth: u8) -> Result<(), SourceError> {
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
            walk(&entry.path(), &relative, files, depth + 1)?;
        } else {
            add(&entry.path(), &relative, files)?;
        }
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

impl SourceSnapshot {
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
        ] {
            optional_walk(root, Path::new(path), &mut files)?;
        }
        parents(root, Path::new("examples/browser"))?;
        browser(&root.join("examples/browser"), &mut files)?;
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
