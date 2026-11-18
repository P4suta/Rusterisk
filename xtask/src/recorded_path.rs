use std::{io, path::Path};

#[derive(Clone, Copy)]
pub(crate) enum RecordedPathFormat {
    Unix,
    Windows,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RecordedPath {
    drive: Option<u8>,
    components: Vec<String>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid recorded absolute path")
}

fn device(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || stem
        .strip_prefix("com")
        .or_else(|| stem.strip_prefix("lpt"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

impl RecordedPathFormat {
    pub(crate) fn parse(self, path: &Path, limit: usize) -> io::Result<RecordedPath> {
        let value = path.to_str().ok_or_else(invalid)?;
        if value.len() > limit || value.chars().any(char::is_control) {
            return Err(invalid());
        }
        match self {
            Self::Unix => {
                let path = value.strip_prefix('/').ok_or_else(invalid)?;
                let mut components = Vec::new();
                for name in path.split('/') {
                    match name {
                        ".." => return Err(invalid()),
                        "" | "." => {}
                        _ => components.push(name.to_owned()),
                    }
                }
                Ok(RecordedPath {
                    drive: None,
                    components,
                })
            }
            Self::Windows => {
                let value = value.strip_prefix(r"\\?\").unwrap_or(value);
                let (drive, path) = value.split_once(":\\").ok_or_else(invalid)?;
                let [drive] = drive.as_bytes() else {
                    return Err(invalid());
                };
                if !drive.is_ascii_alphabetic() || path.ends_with('\\') {
                    return Err(invalid());
                }
                let mut components = Vec::new();
                for name in path.split_terminator('\\') {
                    if name.is_empty()
                        || name.ends_with(['.', ' '])
                        || name
                            .chars()
                            .any(|character| "<>:\"/|?*".contains(character))
                        || device(name)
                    {
                        return Err(invalid());
                    }
                    components.push(name.to_ascii_lowercase());
                }
                Ok(RecordedPath {
                    drive: Some(drive.to_ascii_lowercase()),
                    components,
                })
            }
        }
    }
}

impl RecordedPath {
    pub(crate) fn join(&self, relative: &str) -> Self {
        let mut path = self.clone();
        path.components.extend(relative.split('/').map(|name| {
            if self.drive.is_some() {
                name.to_ascii_lowercase()
            } else {
                name.to_owned()
            }
        }));
        path
    }

    pub(crate) fn starts_with(&self, root: &Self) -> bool {
        self.drive == root.drive && self.components.starts_with(&root.components)
    }

    pub(crate) fn file_name(&self) -> Option<&str> {
        self.components.last().map(String::as_str)
    }

    pub(crate) fn parent(&self) -> Option<Self> {
        let mut parent = self.clone();
        parent.components.pop()?;
        Some(parent)
    }
}
