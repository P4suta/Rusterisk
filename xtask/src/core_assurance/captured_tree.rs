use super::{AssuranceError, CapturedAssurance, FieldMutation};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Debug)]
pub(crate) enum AssuranceTreeError {
    Io(io::Error),
    Assurance(AssuranceError),
    Changed,
    Plan,
}
impl std::fmt::Display for AssuranceTreeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Assurance(error) => error.fmt(formatter),
            Self::Changed => formatter.write_str("captured or normalized assurance inputs changed"),
            Self::Plan => formatter.write_str("invalid bounded assurance input tree"),
        }
    }
}
impl std::error::Error for AssuranceTreeError {}
impl From<io::Error> for AssuranceTreeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<AssuranceError> for AssuranceTreeError {
    fn from(error: AssuranceError) -> Self {
        Self::Assurance(error)
    }
}

fn directory(path: &Path) -> Result<(), AssuranceTreeError> {
    if fs::symlink_metadata(path)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(AssuranceTreeError::Changed)
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
fn write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), AssuranceTreeError> {
    let path = root.join(path);
    fs::create_dir_all(path.parent().ok_or(AssuranceTreeError::Plan)?)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}
pub(crate) struct CapturedAssuranceTree {
    snapshot: CapturedAssurance,
    paths: Vec<PathBuf>,
}
impl CapturedAssuranceTree {
    pub(crate) fn read(root: &Path) -> Result<Self, AssuranceTreeError> {
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
            return Err(AssuranceTreeError::Plan);
        }
        let paths = snapshot
            .inputs()
            .map(|(path, _)| Path::new("crates/core").join(path))
            .collect();
        Ok(Self { snapshot, paths })
    }
    pub(crate) fn inputs(&self) -> impl ExactSizeIterator<Item = (&Path, &[u8])> {
        self.paths
            .iter()
            .zip(self.snapshot.inputs())
            .map(|(project, (_, bytes))| (project.as_path(), bytes))
    }
    pub(crate) fn targets(&self) -> &BTreeSet<String> {
        self.snapshot.targets()
    }
    pub(crate) fn tests(&self) -> &BTreeSet<String> {
        self.snapshot.tests()
    }
    fn lock(&self) -> Result<&[u8], AssuranceTreeError> {
        self.snapshot
            .inputs()
            .find(|(path, _)| *path == Path::new("assurance/Cargo.lock"))
            .map(|(_, bytes)| bytes)
            .ok_or(AssuranceTreeError::Plan)
    }
    pub(crate) fn write(&self, original: &Path, working: &Path) -> Result<(), AssuranceTreeError> {
        for (path, bytes) in self.inputs() {
            write(original, path, bytes)?;
        }
        self.write_working(working, None)
    }
    fn field_source(&self, mutation: FieldMutation) -> Result<String, AssuranceTreeError> {
        let bytes = self
            .snapshot
            .inputs()
            .find(|(path, _)| *path == Path::new("src/lib.rs"))
            .map(|(_, bytes)| bytes)
            .ok_or(AssuranceTreeError::Plan)?;
        let source = std::str::from_utf8(bytes).map_err(|_| AssuranceTreeError::Plan)?;
        Ok(mutation.apply(source)?)
    }
    pub(crate) fn write_field(
        &self,
        working: &Path,
        mutation: FieldMutation,
    ) -> Result<(), AssuranceTreeError> {
        let field = self.field_source(mutation)?;
        self.write_working(working, Some(field.as_bytes()))
    }
    fn write_working(
        &self,
        working: &Path,
        field: Option<&[u8]>,
    ) -> Result<(), AssuranceTreeError> {
        for (path, bytes) in self.snapshot.inputs() {
            if path.starts_with("src") || path.starts_with("tests") {
                let bytes = if path == Path::new("src/lib.rs") {
                    field.unwrap_or(bytes)
                } else {
                    bytes
                };
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
    pub(crate) fn check(&self, original: &Path, working: &Path) -> Result<(), AssuranceTreeError> {
        self.check_original(original)?;
        self.check_working(working, None)
    }
    pub(crate) fn check_field(
        &self,
        original: &Path,
        working: &Path,
        mutation: FieldMutation,
    ) -> Result<(), AssuranceTreeError> {
        self.check_original(original)?;
        let field = self.field_source(mutation)?;
        self.check_working(working, Some(field.as_bytes()))
    }
    fn check_original(&self, original: &Path) -> Result<(), AssuranceTreeError> {
        let actual = Self::read(original)?;
        if !actual.inputs().eq(self.inputs())
            || actual.snapshot.normalized() != self.snapshot.normalized()
            || actual.targets() != self.targets()
            || actual.tests() != self.tests()
        {
            return Err(AssuranceTreeError::Changed);
        }
        Ok(())
    }
    fn check_working(
        &self,
        working: &Path,
        field: Option<&[u8]>,
    ) -> Result<(), AssuranceTreeError> {
        let mut expected: BTreeMap<_, _> = self
            .snapshot
            .inputs()
            .filter(|(path, _)| path.starts_with("src") || path.starts_with("tests"))
            .map(|(path, bytes)| (path.to_owned(), bytes))
            .collect();
        if let Some(field) = field {
            expected.insert("src/lib.rs".into(), field);
        }
        expected.insert("Cargo.toml".into(), self.snapshot.normalized().as_bytes());
        expected.insert("Cargo.lock".into(), self.lock()?);
        check_tree(working, Path::new(""), 0, &mut expected)?;
        if !expected.is_empty() {
            return Err(AssuranceTreeError::Changed);
        }
        Ok(())
    }
}
fn check_tree(
    root: &Path,
    relative: &Path,
    depth: usize,
    expected: &mut BTreeMap<PathBuf, &[u8]>,
) -> Result<(), AssuranceTreeError> {
    if depth > 9 {
        return Err(AssuranceTreeError::Changed);
    }
    directory(&root.join(relative))?;
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = relative.join(entry.file_name());
        if !normal(&path) || kind.is_symlink() {
            return Err(AssuranceTreeError::Changed);
        }
        if kind.is_dir() {
            if !expected.keys().any(|expected| expected.starts_with(&path)) {
                return Err(AssuranceTreeError::Changed);
            }
            check_tree(root, &path, depth + 1, expected)?;
        } else if kind.is_file() {
            let bytes = expected.remove(&path).ok_or(AssuranceTreeError::Changed)?;
            if entry.metadata()?.len() != bytes.len() as u64 {
                return Err(AssuranceTreeError::Changed);
            }
            let mut actual = Vec::new();
            fs::File::open(entry.path())?
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut actual)?;
            if actual != bytes {
                return Err(AssuranceTreeError::Changed);
            }
        } else {
            return Err(AssuranceTreeError::Changed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
