use super::{StoragePort, WorkerError};
#[cfg(test)]
use rusterisk_config::MusicClassName;
use rusterisk_config::{ConfigError, MusicFilesClass, MusicOnHold};
use rusterisk_storage::{StorageError, WaveReader};
use std::cmp::Ordering;
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::oneshot;

mod loop_reader;

pub(crate) use loop_reader::MusicCursor;
pub(super) use loop_reader::{MusicBuffer, MusicReader};

const MAX_CLASS_TRACKS: usize = 64;
const MAX_TOTAL_TRACKS: usize = 256;
const MAX_ENTRIES: usize = 512;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct PreparedMusic {
    catalog: MusicCatalog,
}

#[derive(Clone, Debug)]
pub struct MusicSelection {
    root: PathBuf,
    class: PreparedClass,
}

impl PreparedMusic {
    pub(crate) fn select(&self, name: &str) -> Option<MusicSelection> {
        let MusicCatalog::Files(inventory) = &self.catalog else {
            return None;
        };
        let class = inventory
            .classes
            .iter()
            .find(|class| class.definition.name().as_str().eq_ignore_ascii_case(name))?;
        Some(MusicSelection {
            root: inventory.root.clone(),
            class: class.clone(),
        })
    }
}

impl MusicSelection {
    pub(crate) fn class_name(&self) -> &str {
        self.class.definition.name().as_str()
    }

    pub(super) fn open(
        &self,
        cursor: Option<MusicCursor>,
    ) -> Result<(WaveReader, usize), StorageError> {
        let cursor = cursor.unwrap_or_else(|| MusicCursor::at(0, 0));
        let track = self
            .class
            .tracks
            .get(cursor.track)
            .ok_or(StorageError::InvalidState)?;
        let checked = || -> Result<WaveReader, MusicPreparationError> {
            let definition = &self.class.definition;
            let mut directory = self.root.clone();
            let metadata = std::fs::symlink_metadata(&directory)
                .map_err(|error| position(definition, error))?;
            check_directory(&metadata, definition)?;
            for component in definition.directory().as_str().split('/') {
                directory.push(component);
                let metadata = std::fs::symlink_metadata(&directory)
                    .map_err(|error| position(definition, error))?;
                check_directory(&metadata, definition)?;
            }
            let path = directory.join(&track.name);
            check_track_path(
                &path,
                &self.root,
                definition,
                &track.stamp,
                &track.canonical,
            )?;
            let file = File::open(&path).map_err(|error| position(definition, error))?;
            let metadata = file
                .metadata()
                .map_err(|error| position(definition, error))?;
            if !metadata.is_file()
                || FileStamp::read(&metadata).map_err(|error| position(definition, error))?
                    != track.stamp
            {
                return Err(position(definition, "music file metadata changed"));
            }
            let mut reader = WaveReader::from_file(file)
                .map_err(|error| position(definition, format!("invalid wave file: {error}")))?;
            let metadata = reader
                .metadata()
                .map_err(|error| position(definition, error))?;
            if reader.rate() != track.rate
                || reader.sample_count() != track.samples
                || metadata.len() != track.encoded_length
                || FileStamp::read(&metadata).map_err(|error| position(definition, error))?
                    != track.stamp
                || cursor.sample > track.samples
            {
                return Err(position(definition, "music file metadata changed"));
            }
            reader
                .seek_sample(cursor.sample)
                .map_err(|error| position(definition, error))?;
            Ok(reader)
        };
        checked()
            .map(|reader| (reader, cursor.track))
            .map_err(|error| StorageError::Io(std::io::Error::other(error)))
    }
}

#[derive(Clone, Debug)]
enum MusicCatalog {
    Empty,
    Files(MusicInventory),
}

#[derive(Clone, Debug)]
struct MusicInventory {
    root: PathBuf,
    classes: Vec<PreparedClass>,
}

#[derive(Clone, Debug)]
struct PreparedClass {
    definition: MusicFilesClass,
    tracks: Vec<PreparedTrack>,
}

#[derive(Clone, Debug)]
struct PreparedTrack {
    name: String,
    rate: u32,
    samples: u64,
    encoded_length: u64,
    stamp: FileStamp,
    canonical: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
}

impl FileStamp {
    fn read(metadata: &Metadata) -> Result<Self, std::io::Error> {
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

struct DirectoryObservation<'a> {
    path: PathBuf,
    canonical: PathBuf,
    modified: SystemTime,
    definition: &'a MusicFilesClass,
}

struct CandidateClass {
    definition: MusicFilesClass,
    tracks: Vec<CandidateTrack>,
}

struct CandidateTrack {
    name: String,
    stamp: FileStamp,
    canonical: PathBuf,
}

#[derive(Default)]
struct Budget {
    entries: usize,
    tracks: usize,
    bytes: u64,
}

#[derive(Debug)]
pub(crate) enum MusicPreparationError {
    Worker(WorkerError),
    Configuration(ConfigError),
}

impl std::fmt::Display for MusicPreparationError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Worker(error) => write!(output, "{error}"),
            Self::Configuration(error) => write!(output, "{error}"),
        }
    }
}

impl std::error::Error for MusicPreparationError {}

enum PreparationCheckpoint {
    TrackValidated {
        #[cfg(test)]
        class: MusicClassName,
        #[cfg(test)]
        name: String,
    },
    BeforePublication,
}

impl StoragePort {
    pub(crate) async fn prepare_music(
        &self,
        classes: MusicOnHold,
    ) -> Result<PreparedMusic, MusicPreparationError> {
        let (reply, response) = oneshot::channel();
        self.send_stored(super::StoredCommand::PrepareMusic { classes, reply })
            .map_err(MusicPreparationError::Worker)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|_| MusicPreparationError::Worker(WorkerError::Timeout))?
            .map_err(|_| MusicPreparationError::Worker(WorkerError::Closed))?
    }
}

fn position(definition: &MusicFilesClass, reason: impl std::fmt::Display) -> MusicPreparationError {
    MusicPreparationError::Configuration(ConfigError {
        file: "musiconhold.conf".into(),
        line: definition.directory_line(),
        reason: format!("music class {}: {reason}", definition.name().as_str()),
    })
}

fn service(
    pump: &mut impl FnMut() -> Result<(), StorageError>,
) -> Result<(), MusicPreparationError> {
    pump().map_err(|error| MusicPreparationError::Worker(WorkerError::Storage(error)))
}

pub(super) fn prepare(
    root: &Path,
    classes: MusicOnHold,
    pump: impl FnMut() -> Result<(), StorageError>,
) -> Result<PreparedMusic, MusicPreparationError> {
    prepare_observed(root, classes, pump, |_| Ok(()))
}

fn prepare_observed(
    root: &Path,
    classes: MusicOnHold,
    mut pump: impl FnMut() -> Result<(), StorageError>,
    mut observer: impl FnMut(PreparationCheckpoint) -> Result<(), StorageError>,
) -> Result<PreparedMusic, MusicPreparationError> {
    let Some(first) = classes.classes().first() else {
        return Ok(PreparedMusic {
            catalog: MusicCatalog::Empty,
        });
    };
    service(&mut pump)?;
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| position(first, format!("data directory unavailable: {error}")))?;
    check_directory(&metadata, first)?;
    let anchor = std::fs::canonicalize(root)
        .map_err(|error| position(first, format!("data directory unavailable: {error}")))?;
    if anchor.as_os_str().as_encoded_bytes().len() > 4096 {
        return Err(position(first, "data directory exceeds 4096 bytes"));
    }
    let mut directories = Vec::new();
    observe_directory(root, &anchor, first, &mut directories)?;
    let mut budget = Budget::default();
    let mut candidates = Vec::new();
    for definition in classes.classes() {
        let mut directory = anchor.clone();
        for component in definition.directory().as_str().split('/') {
            service(&mut pump)?;
            directory.push(component);
            observe_directory(&directory, &anchor, definition, &mut directories)?;
        }
        candidates.push(scan_class(
            &directory,
            &anchor,
            definition,
            &mut budget,
            &mut pump,
        )?);
    }
    let mut prepared = Vec::new();
    for candidate in candidates {
        let mut tracks = Vec::new();
        for track in candidate.tracks {
            tracks.push(validate_track(
                &anchor,
                &candidate.definition,
                track,
                &mut pump,
                &mut observer,
            )?);
        }
        order_tracks(&candidate.definition, &mut tracks)?;
        prepared.push(PreparedClass {
            definition: candidate.definition,
            tracks,
        });
    }
    service(&mut pump)?;
    observer(PreparationCheckpoint::BeforePublication)
        .map_err(|error| MusicPreparationError::Worker(WorkerError::Storage(error)))?;
    for directory in directories {
        service(&mut pump)?;
        let metadata = std::fs::symlink_metadata(&directory.path).map_err(|error| {
            position(directory.definition, format!("directory changed: {error}"))
        })?;
        let canonical = std::fs::canonicalize(&directory.path).map_err(|error| {
            position(directory.definition, format!("directory changed: {error}"))
        })?;
        if linked(&metadata)
            || !metadata.is_dir()
            || canonical != directory.canonical
            || metadata
                .modified()
                .map_err(|error| position(directory.definition, error))?
                != directory.modified
        {
            return Err(position(directory.definition, "directory metadata changed"));
        }
    }
    for class in &prepared {
        for track in &class.tracks {
            service(&mut pump)?;
            check_track_path(
                &track_path(&anchor, &class.definition, &track.name),
                &anchor,
                &class.definition,
                &track.stamp,
                &track.canonical,
            )?;
        }
    }
    Ok(PreparedMusic {
        catalog: MusicCatalog::Files(MusicInventory {
            root: anchor,
            classes: prepared,
        }),
    })
}

fn check_directory(
    metadata: &Metadata,
    definition: &MusicFilesClass,
) -> Result<(), MusicPreparationError> {
    if linked(metadata) {
        return Err(position(
            definition,
            "symbolic link or reparse point in music directory",
        ));
    }
    if !metadata.is_dir() {
        return Err(position(definition, "music directory is not a directory"));
    }
    Ok(())
}

fn observe_directory<'a>(
    path: &Path,
    root: &Path,
    definition: &'a MusicFilesClass,
    observations: &mut Vec<DirectoryObservation<'a>>,
) -> Result<(), MusicPreparationError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| position(definition, format!("directory unavailable: {error}")))?;
    check_directory(&metadata, definition)?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| position(definition, format!("directory unavailable: {error}")))?;
    if !canonical.starts_with(root) {
        return Err(position(definition, "directory leaves the data root"));
    }
    let modified = metadata
        .modified()
        .map_err(|error| position(definition, error))?;
    if let Some(previous) = observations.iter().find(|previous| previous.path == path) {
        if previous.canonical != canonical || previous.modified != modified {
            return Err(position(definition, "directory metadata changed"));
        }
    } else {
        observations.push(DirectoryObservation {
            path: path.to_owned(),
            canonical,
            modified,
            definition,
        });
    }
    Ok(())
}

fn scan_class(
    directory: &Path,
    root: &Path,
    definition: &MusicFilesClass,
    budget: &mut Budget,
    pump: &mut impl FnMut() -> Result<(), StorageError>,
) -> Result<CandidateClass, MusicPreparationError> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| position(definition, format!("directory unavailable: {error}")))?;
    let mut tracks = Vec::new();
    for entry in entries {
        service(pump)?;
        budget.entries = budget
            .entries
            .checked_add(1)
            .filter(|count| *count <= MAX_ENTRIES)
            .ok_or_else(|| position(definition, "512 directory entries exceeded"))?;
        let entry = entry.map_err(|error| position(definition, error))?;
        let metadata =
            std::fs::symlink_metadata(entry.path()).map_err(|error| position(definition, error))?;
        if linked(&metadata) {
            return Err(position(
                definition,
                "symbolic link or reparse point in music directory",
            ));
        }
        let filename = entry.file_name();
        if filename.as_encoded_bytes().starts_with(b".")
            || entry.path().extension() != Some(std::ffi::OsStr::new("wav"))
        {
            continue;
        }
        if !metadata.is_file() {
            return Err(position(definition, "wave asset must be a regular file"));
        }
        let name = filename
            .to_str()
            .filter(|name| {
                name.len() <= 255
                    && !name.contains(['/', '\\', ':'])
                    && !name.chars().any(char::is_control)
            })
            .ok_or_else(|| position(definition, "invalid music file name"))?;
        if tracks.len() >= MAX_CLASS_TRACKS {
            return Err(position(definition, "64 wave files per class exceeded"));
        }
        budget.tracks = budget
            .tracks
            .checked_add(1)
            .filter(|count| *count <= MAX_TOTAL_TRACKS)
            .ok_or_else(|| position(definition, "256 total wave references exceeded"))?;
        let stamp = FileStamp::read(&metadata).map_err(|error| position(definition, error))?;
        if stamp.length > MAX_FILE_BYTES {
            return Err(position(definition, "16 MiB music file limit exceeded"));
        }
        budget.bytes = budget
            .bytes
            .checked_add(stamp.length)
            .filter(|bytes| *bytes <= MAX_TOTAL_BYTES)
            .ok_or_else(|| position(definition, "64 MiB total music limit exceeded"))?;
        let canonical =
            std::fs::canonicalize(entry.path()).map_err(|error| position(definition, error))?;
        if !canonical.starts_with(root) {
            return Err(position(definition, "music file leaves the data root"));
        }
        tracks.push(CandidateTrack {
            name: name.into(),
            stamp,
            canonical,
        });
    }
    if tracks.is_empty() {
        return Err(position(definition, "empty music class"));
    }
    Ok(CandidateClass {
        definition: definition.clone(),
        tracks,
    })
}

fn track_path(root: &Path, definition: &MusicFilesClass, name: &str) -> PathBuf {
    let mut path = root.to_owned();
    for component in definition.directory().as_str().split('/') {
        path.push(component);
    }
    path.push(name);
    path
}

fn check_track_path(
    path: &Path,
    root: &Path,
    definition: &MusicFilesClass,
    expected: &FileStamp,
    canonical: &Path,
) -> Result<(), MusicPreparationError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| position(definition, format!("music file changed: {error}")))?;
    let current = std::fs::canonicalize(path)
        .map_err(|error| position(definition, format!("music file changed: {error}")))?;
    let stamp = FileStamp::read(&metadata).map_err(|error| position(definition, error))?;
    if linked(&metadata)
        || !metadata.is_file()
        || &stamp != expected
        || current != canonical
        || !current.starts_with(root)
    {
        return Err(position(definition, "music file metadata changed"));
    }
    Ok(())
}

fn validate_track(
    root: &Path,
    definition: &MusicFilesClass,
    candidate: CandidateTrack,
    pump: &mut impl FnMut() -> Result<(), StorageError>,
    observer: &mut impl FnMut(PreparationCheckpoint) -> Result<(), StorageError>,
) -> Result<PreparedTrack, MusicPreparationError> {
    service(pump)?;
    let path = track_path(root, definition, &candidate.name);
    check_track_path(
        &path,
        root,
        definition,
        &candidate.stamp,
        &candidate.canonical,
    )?;
    let file = File::open(&path)
        .map_err(|error| position(definition, format!("wave file unavailable: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| position(definition, error))?;
    if !metadata.is_file()
        || FileStamp::read(&metadata).map_err(|error| position(definition, error))?
            != candidate.stamp
    {
        return Err(position(definition, "music file metadata changed"));
    }
    let mut reader = WaveReader::from_file(file)
        .map_err(|error| position(definition, format!("invalid wave file: {error}")))?;
    let rate = reader.rate();
    let samples = reader.sample_count();
    if samples == 0 {
        return Err(position(definition, "empty wave audio"));
    }
    let encoded_length = samples
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(44))
        .ok_or_else(|| position(definition, "wave length overflow"))?;
    if encoded_length != candidate.stamp.length {
        return Err(position(definition, "music file metadata changed"));
    }
    let mut consumed = 0u64;
    loop {
        service(pump)?;
        let Some(frame) = reader
            .read_frame()
            .map_err(|error| position(definition, format!("invalid wave audio: {error}")))?
        else {
            break;
        };
        consumed = consumed
            .checked_add(frame.samples().len() as u64)
            .filter(|count| *count <= samples)
            .ok_or_else(|| position(definition, "wave sample count exceeded"))?;
    }
    if consumed != samples {
        return Err(position(definition, "wave sample count mismatch"));
    }
    let metadata = reader
        .metadata()
        .map_err(|error| position(definition, error))?;
    if !metadata.is_file()
        || FileStamp::read(&metadata).map_err(|error| position(definition, error))?
            != candidate.stamp
    {
        return Err(position(definition, "music file metadata changed"));
    }
    drop(reader);
    observer(PreparationCheckpoint::TrackValidated {
        #[cfg(test)]
        class: definition.name().clone(),
        #[cfg(test)]
        name: candidate.name.clone(),
    })
    .map_err(|error| MusicPreparationError::Worker(WorkerError::Storage(error)))?;
    Ok(PreparedTrack {
        name: candidate.name,
        rate,
        samples,
        encoded_length,
        stamp: candidate.stamp,
        canonical: candidate.canonical,
    })
}

fn order_tracks(
    definition: &MusicFilesClass,
    tracks: &mut [PreparedTrack],
) -> Result<(), MusicPreparationError> {
    tracks.sort_by(|left, right| compare_names(&left.name, &right.name));
    if tracks
        .windows(2)
        .any(|pair| pair[0].name.eq_ignore_ascii_case(&pair[1].name))
    {
        return Err(position(definition, "case-ambiguous music file names"));
    }
    Ok(())
}

fn compare_names(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
        .then_with(|| left.cmp(right))
}

fn linked(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        reparse_attributes(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(any(windows, test))]
fn reparse_attributes(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(test)]
mod tests;
