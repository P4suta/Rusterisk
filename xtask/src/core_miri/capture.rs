use super::CoreMiriError;
use crate::core_assurance::CapturedAssurance;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

fn directory(path: &Path) -> Result<(), CoreMiriError> {
    if fs::symlink_metadata(path)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(CoreMiriError::Changed)
    }
}
fn normal(path: &Path) -> bool {
    path.components().all(|component| match component {
        Component::Normal(value) => value.to_str().is_some_and(|value| {
            !value
                .chars()
                .any(|character| character.is_control() || character == '\\')
        }),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    })
}
fn write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), CoreMiriError> {
    let path = root.join(path);
    fs::create_dir_all(path.parent().ok_or(CoreMiriError::Plan)?)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}
pub struct CapturedCoreMiri {
    snapshot: CapturedAssurance,
    paths: Vec<PathBuf>,
}
impl CapturedCoreMiri {
    pub fn read(root: &Path) -> Result<Self, CoreMiriError> {
        directory(root)?;
        for path in [
            "crates",
            "crates/core",
            "crates/core/src",
            "crates/core/tests",
            "crates/core/assurance",
        ] {
            directory(&root.join(path))?;
        }
        let snapshot = CapturedAssurance::read(&root.join("crates/core"))?;
        if snapshot.normalized().len() > 65_536 || snapshot.inputs().any(|(path, _)| !normal(path))
        {
            return Err(CoreMiriError::Plan);
        }
        let paths = snapshot
            .inputs()
            .map(|(path, _)| Path::new("crates/core").join(path))
            .collect();
        Ok(Self { snapshot, paths })
    }
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.paths
            .iter()
            .zip(self.snapshot.inputs())
            .map(|(project, (_, bytes))| (project.as_path(), bytes))
    }
    pub fn targets(&self) -> &BTreeSet<String> {
        self.snapshot.targets()
    }
    pub fn tests(&self) -> &BTreeSet<String> {
        self.snapshot.tests()
    }
    fn lock(&self) -> Result<&[u8], CoreMiriError> {
        self.snapshot
            .inputs()
            .find(|(path, _)| *path == Path::new("assurance/Cargo.lock"))
            .map(|(_, bytes)| bytes)
            .ok_or(CoreMiriError::Plan)
    }
    pub(super) fn write(&self, original: &Path, working: &Path) -> Result<(), CoreMiriError> {
        for (path, bytes) in self.inputs() {
            write(original, path, bytes)?;
        }
        for (path, bytes) in self.snapshot.inputs() {
            if path.starts_with("src") || path.starts_with("tests") {
                write(working, path, bytes)?;
            }
        }
        write(
            working,
            Path::new("Cargo.toml"),
            self.snapshot.normalized().as_bytes(),
        )?;
        write(working, Path::new("Cargo.lock"), self.lock()?)
    }
    pub(super) fn check(&self, original: &Path, working: &Path) -> Result<(), CoreMiriError> {
        let actual = Self::read(original)?;
        if !actual.inputs().eq(self.inputs())
            || actual.snapshot.normalized() != self.snapshot.normalized()
            || actual.targets() != self.targets()
            || actual.tests() != self.tests()
        {
            return Err(CoreMiriError::Changed);
        }
        let mut expected: BTreeMap<_, _> = self
            .snapshot
            .inputs()
            .filter(|(path, _)| path.starts_with("src") || path.starts_with("tests"))
            .map(|(path, bytes)| (path.to_owned(), bytes))
            .collect();
        expected.insert("Cargo.toml".into(), self.snapshot.normalized().as_bytes());
        expected.insert("Cargo.lock".into(), self.lock()?);
        check_tree(working, Path::new(""), 0, &mut expected)?;
        if !expected.is_empty() {
            return Err(CoreMiriError::Changed);
        }
        Ok(())
    }
}
fn check_tree(
    root: &Path,
    relative: &Path,
    depth: usize,
    expected: &mut BTreeMap<PathBuf, &[u8]>,
) -> Result<(), CoreMiriError> {
    if depth > 9 {
        return Err(CoreMiriError::Changed);
    }
    directory(&root.join(relative))?;
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = relative.join(entry.file_name());
        if !normal(&path) || kind.is_symlink() {
            return Err(CoreMiriError::Changed);
        }
        if kind.is_dir() {
            if !expected.keys().any(|expected| expected.starts_with(&path)) {
                return Err(CoreMiriError::Changed);
            }
            check_tree(root, &path, depth + 1, expected)?;
        } else if kind.is_file() {
            let bytes = expected.remove(&path).ok_or(CoreMiriError::Changed)?;
            if entry.metadata()?.len() != bytes.len() as u64 {
                return Err(CoreMiriError::Changed);
            }
            let mut actual = Vec::new();
            fs::File::open(entry.path())?
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut actual)?;
            if actual != bytes {
                return Err(CoreMiriError::Changed);
            }
        } else {
            return Err(CoreMiriError::Changed);
        }
    }
    Ok(())
}
