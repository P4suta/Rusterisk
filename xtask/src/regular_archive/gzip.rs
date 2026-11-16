use super::{ArchivePath, ArchivePathError, ArchiveRoot};
use std::{
    collections::BTreeSet,
    io::{BufRead, Read, Take},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveLimit {
    Compressed,
    Expanded,
    File,
    Entries,
}

#[derive(Debug)]
pub enum RegularArchiveError {
    Io(std::io::Error),
    Path(ArchivePathError),
    InvalidLimits,
    Limit(ArchiveLimit),
    UnsupportedEntry,
    Duplicate,
    Hierarchy,
    Termination,
    Trailing,
    Empty,
}

impl std::fmt::Display for RegularArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Path(error) => error.fmt(formatter),
            Self::InvalidLimits => formatter.write_str("invalid bounded archive limits"),
            Self::Limit(limit) => write!(formatter, "archive exceeds the {limit:?} limit"),
            Self::UnsupportedEntry => {
                formatter.write_str("archive profile permits only regular tar entries")
            }
            Self::Duplicate => formatter.write_str("archive contains duplicate destination names"),
            Self::Hierarchy => {
                formatter.write_str("archive file conflicts with a parent directory")
            }
            Self::Termination => {
                formatter.write_str("archive has invalid tar termination or padding")
            }
            Self::Trailing => formatter
                .write_str("archive profile requires one gzip member without trailing bytes"),
            Self::Empty => formatter.write_str("archive must contain regular files"),
        }
    }
}

impl std::error::Error for RegularArchiveError {}

impl From<std::io::Error> for RegularArchiveError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ArchivePathError> for RegularArchiveError {
    fn from(error: ArchivePathError) -> Self {
        Self::Path(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    compressed: u64,
    expanded: u64,
    file: u64,
    entries: usize,
    depth: usize,
}

impl ArchiveLimits {
    pub const SERVER: Self = Self {
        compressed: 272 * 1024 * 1024,
        expanded: 272 * 1024 * 1024,
        file: 256 * 1024 * 1024,
        entries: 9,
        depth: 2,
    };

    pub const SDK: Self = Self {
        compressed: 16 * 1024 * 1024,
        expanded: 32 * 1024 * 1024,
        file: 8 * 1024 * 1024,
        entries: 1024,
        depth: 16,
    };

    pub fn constrain(
        self,
        compressed: u64,
        expanded: u64,
        file: u64,
        entries: usize,
        depth: usize,
    ) -> Result<Self, RegularArchiveError> {
        if compressed == 0
            || compressed > self.compressed
            || expanded == 0
            || expanded > self.expanded
            || file == 0
            || file > self.file
            || file > expanded
            || entries == 0
            || entries > self.entries
            || depth == 0
            || depth > self.depth
        {
            return Err(RegularArchiveError::InvalidLimits);
        }
        Ok(Self {
            compressed,
            expanded,
            file,
            entries,
            depth,
        })
    }
}

struct Inventory {
    files: Vec<ArchiveFile>,
    data_bytes: u64,
}

fn read_entries<R: Read>(
    archive: &mut tar::Archive<R>,
    root: &ArchiveRoot,
    limits: ArchiveLimits,
) -> Result<Inventory, RegularArchiveError> {
    let mut inventory = Inventory {
        files: Vec::new(),
        data_bytes: 0,
    };
    let mut names = BTreeSet::new();
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err(RegularArchiveError::UnsupportedEntry);
        }
        if inventory.files.len() >= limits.entries {
            return Err(RegularArchiveError::Limit(ArchiveLimit::Entries));
        }
        let size = entry.header().size()?;
        if size > limits.file {
            return Err(RegularArchiveError::Limit(ArchiveLimit::File));
        }
        let path = ArchivePath::parse(root, &entry.path_bytes(), limits.depth)?;
        let key = path.destination_key();
        if names.contains(&key) {
            return Err(RegularArchiveError::Duplicate);
        }
        let prefix = format!("{key}/");
        if key
            .match_indices('/')
            .any(|(index, _)| names.contains(&key[..index]))
            || names
                .range(prefix.clone()..)
                .next()
                .is_some_and(|existing: &String| existing.starts_with(&prefix))
        {
            return Err(RegularArchiveError::Hierarchy);
        }
        if !names.insert(key) {
            return Err(RegularArchiveError::Duplicate);
        }
        inventory.data_bytes = inventory
            .data_bytes
            .checked_add(size)
            .filter(|total| *total <= limits.expanded)
            .ok_or(RegularArchiveError::Limit(ArchiveLimit::Expanded))?;
        let mut bytes = Vec::new();
        entry.by_ref().take(size + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size {
            return Err(RegularArchiveError::Termination);
        }
        inventory.files.push(ArchiveFile { path, bytes });
    }
    Ok(inventory)
}

type BoundedGzip<R> = Take<flate2::bufread::GzDecoder<Take<R>>>;

fn bounded_error<R: BufRead>(
    stream: &BoundedGzip<R>,
    error: RegularArchiveError,
) -> RegularArchiveError {
    if stream.limit() == 0 {
        RegularArchiveError::Limit(ArchiveLimit::Expanded)
    } else if stream.get_ref().get_ref().limit() == 0 {
        RegularArchiveError::Limit(ArchiveLimit::Compressed)
    } else {
        error
    }
}

#[derive(Debug)]
pub struct ArchiveFile {
    path: ArchivePath,
    bytes: Vec<u8>,
}

impl ArchiveFile {
    pub fn path(&self) -> &ArchivePath {
        &self.path
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug)]
pub struct RegularArchive {
    files: Vec<ArchiveFile>,
    expanded: u64,
}

impl RegularArchive {
    pub fn files(&self) -> &[ArchiveFile] {
        &self.files
    }
    pub fn expanded_bytes(&self) -> u64 {
        self.expanded
    }
}

pub fn read_regular_gzip(
    input: impl BufRead,
    root: &ArchiveRoot,
    limits: ArchiveLimits,
) -> Result<RegularArchive, RegularArchiveError> {
    let decoder = flate2::bufread::GzDecoder::new(input.take(limits.compressed + 1));
    let mut archive = tar::Archive::new(decoder.take(limits.expanded + 1));
    let operation = read_entries(&mut archive, root, limits);
    let mut decoded = archive.into_inner();
    let inventory = operation.map_err(|error| bounded_error(&decoded, error))?;
    let mut tail = 0u64;
    let mut buffer = [0; 8192];
    loop {
        let count = decoded
            .read(&mut buffer)
            .map_err(|error| bounded_error(&decoded, error.into()))?;
        if count == 0 {
            break;
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(RegularArchiveError::Termination);
        }
        tail += count as u64;
    }
    if decoded.limit() == 0 {
        return Err(RegularArchiveError::Limit(ArchiveLimit::Expanded));
    }
    let expanded = limits.expanded + 1 - decoded.limit();
    if tail < 512 || !expanded.is_multiple_of(512) {
        return Err(RegularArchiveError::Termination);
    }
    let mut compressed = decoded.into_inner().into_inner();
    if compressed.limit() == 0 {
        return Err(RegularArchiveError::Limit(ArchiveLimit::Compressed));
    }
    if !compressed.fill_buf()?.is_empty() {
        return Err(RegularArchiveError::Trailing);
    }
    if inventory.files.is_empty() {
        return Err(RegularArchiveError::Empty);
    }
    Ok(RegularArchive {
        files: inventory.files,
        expanded,
    })
}
