#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchivePathError {
    Root,
    UnexpectedRoot,
    Component,
    NonPortable,
    Depth,
    Limits,
}

impl std::fmt::Display for ArchivePathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Root => "invalid archive root",
            Self::UnexpectedRoot => "archive entry belongs to another root",
            Self::Component => "archive entry has an unsafe path component",
            Self::NonPortable => "archive entry path is not portable",
            Self::Depth => "archive entry exceeds the path depth limit",
            Self::Limits => "invalid archive path depth limit",
        })
    }
}

impl std::error::Error for ArchivePathError {}

fn portable_component(value: &str) -> bool {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return false;
    }
    let base = value
        .split_once('.')
        .map_or(value, |(base, _)| base)
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    for prefix in ["COM", "LPT"] {
        if base
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes(), [b'1'..=b'9']))
        {
            return false;
        }
    }
    true
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveRoot(String);

impl ArchiveRoot {
    pub fn new(value: &str) -> Result<Self, ArchivePathError> {
        if value.len() > 240 || !portable_component(value) {
            return Err(ArchivePathError::Root);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchivePath(String);

impl ArchivePath {
    pub fn parse(
        root: &ArchiveRoot,
        raw: &[u8],
        max_depth: usize,
    ) -> Result<Self, ArchivePathError> {
        if !(1..=16).contains(&max_depth) {
            return Err(ArchivePathError::Limits);
        }
        if raw.len() > 240 {
            return Err(ArchivePathError::NonPortable);
        }
        let source = std::str::from_utf8(raw).map_err(|_| ArchivePathError::NonPortable)?;
        let (prefix, relative) = source.split_once('/').ok_or(ArchivePathError::Component)?;
        if prefix != root.as_str() {
            return Err(ArchivePathError::UnexpectedRoot);
        }
        for (index, component) in relative.split('/').enumerate() {
            if component.is_empty() || component == "." || component == ".." {
                return Err(ArchivePathError::Component);
            }
            if !portable_component(component) {
                return Err(ArchivePathError::NonPortable);
            }
            if index >= max_depth {
                return Err(ArchivePathError::Depth);
            }
        }
        Ok(Self(relative.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn destination_key(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

mod gzip;
pub use gzip::{
    read_regular_gzip, ArchiveFile, ArchiveLimit, ArchiveLimits, RegularArchive,
    RegularArchiveError,
};
