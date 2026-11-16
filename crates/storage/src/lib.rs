use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
mod history;
mod preparation;
mod recording_copy;
mod recording_file;
pub use history::{CallHistoryChange, CallRecord};
pub use preparation::RecordingPreparation;
pub use recording_copy::RecordingCopy;
pub use recording_file::{RecordingChunk, StoredRecordingReader};

const MAX_RECORDING: u64 = 1024 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4096;

#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    PublicationCleanup {
        operation: std::io::Error,
        cleanup: std::io::Error,
    },
    Database(rusqlite::Error),
    Media(rusterisk_media::MediaError),
    InvalidName,
    InvalidWave,
    InvalidState,
    Capacity,
    UnknownSchema,
    InUse,
    Exists,
    NotFound,
    Unsupported,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "recording I/O: {error}"),
            Self::PublicationCleanup { operation, cleanup } => {
                write!(
                    f,
                    "recording publication failed: {operation}, stage cleanup failed: {cleanup}"
                )
            }
            Self::Database(error) => write!(f, "metadata: {error}"),
            Self::Media(error) => write!(f, "{error}"),
            Self::InvalidName => f.write_str("invalid recording or call name"),
            Self::InvalidWave => f.write_str("invalid PCM wave file"),
            Self::InvalidState => f.write_str("invalid storage transition"),
            Self::Capacity => f.write_str("recording capacity exceeded"),
            Self::UnknownSchema => f.write_str("unsupported database schema"),
            Self::InUse => f.write_str("storage directory is already in use"),
            Self::Exists => f.write_str("recording already exists"),
            Self::NotFound => f.write_str("recording not found"),
            Self::Unsupported => f.write_str("unsupported recording operation"),
        }
    }
}

impl std::error::Error for StorageError {}
impl From<std::io::Error> for StorageError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<rusqlite::Error> for StorageError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}
impl From<rusterisk_media::MediaError> for StorageError {
    fn from(value: rusterisk_media::MediaError) -> Self {
        Self::Media(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    Active,
    Completed,
    Interrupted,
}

impl CallOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
        }
    }
    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(StorageError::InvalidState),
        }
    }
}

pub struct Store {
    connection: rusqlite::Connection,
    recordings: PathBuf,
    _lock: File,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        std::fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".rusterisk.lock"))?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => StorageError::InUse,
            std::fs::TryLockError::Error(error) => StorageError::Io(error),
        })?;
        let recordings = root.join("recordings");
        std::fs::create_dir_all(&recordings)?;
        if std::fs::symlink_metadata(&recordings)?
            .file_type()
            .is_symlink()
        {
            return Err(StorageError::InvalidName);
        }
        let mut connection = rusqlite::Connection::open(root.join("calls.sqlite3"))?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                let transaction = connection.transaction()?;
                transaction.execute_batch("CREATE TABLE calls (id TEXT PRIMARY KEY NOT NULL, channel TEXT NOT NULL, generation TEXT NOT NULL, started_at TEXT NOT NULL, ended_at TEXT, outcome TEXT NOT NULL CHECK (outcome IN ('active','completed','interrupted'))); CREATE TABLE recordings (name TEXT PRIMARY KEY NOT NULL, format TEXT NOT NULL, outcome TEXT NOT NULL); PRAGMA user_version=2;")?;
                transaction.commit()?;
            }
            1 => {
                let transaction = connection.transaction()?;
                transaction.execute_batch("ALTER TABLE calls ADD COLUMN channel TEXT NOT NULL DEFAULT ''; ALTER TABLE calls ADD COLUMN generation TEXT NOT NULL DEFAULT '0'; UPDATE calls SET channel=id; PRAGMA user_version=2;")?;
                transaction.commit()?;
            }
            2 => {}
            _ => return Err(StorageError::UnknownSchema),
        }
        connection.execute("UPDATE calls SET outcome='interrupted', ended_at=COALESCE(ended_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE outcome='active'", [])?;
        let mut count = 0usize;
        let mut interrupted = Vec::new();
        for entry in std::fs::read_dir(&recordings)? {
            let entry = entry?;
            count += 1;
            if count > MAX_DIRECTORY_ENTRIES {
                return Err(StorageError::Capacity);
            }
            if entry.file_type()?.is_symlink() {
                return Err(StorageError::InvalidName);
            }
            if entry.path().extension().is_some_and(|value| {
                value == "partial"
                    || value == "replacement"
                    || value == "append"
                    || value == "copying"
            }) {
                interrupted.push(entry.path());
            }
        }
        for path in interrupted {
            if path.extension().is_some_and(|value| value == "copying") {
                std::fs::remove_file(path)?;
                continue;
            }
            if !recover(&path)? {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or(StorageError::InvalidName)?;
            connection.execute("INSERT INTO recordings (name,format,outcome) VALUES (?1,'wav','interrupted') ON CONFLICT(name) DO UPDATE SET outcome='interrupted'", [name])?;
        }
        Ok(Self {
            connection,
            recordings,
            _lock: lock,
        })
    }

    pub fn start_call(&mut self, id: &str, timestamp: &str) -> Result<(), StorageError> {
        validate_name(id)?;
        if timestamp.len() > 64 {
            return Err(StorageError::Capacity);
        }
        self.connection.execute(
            "INSERT INTO calls (id,channel,generation,started_at,outcome) VALUES (?1,?1,'0',?2,'active')",
            (id, timestamp),
        )?;
        Ok(())
    }

    pub fn finish_call(
        &mut self,
        id: &str,
        outcome: CallOutcome,
        timestamp: &str,
    ) -> Result<(), StorageError> {
        if outcome == CallOutcome::Active || timestamp.len() > 64 {
            return Err(StorageError::InvalidState);
        }
        let updated = self.connection.execute(
            "UPDATE calls SET outcome=?1, ended_at=?2 WHERE id=?3 AND outcome='active'",
            (outcome.as_str(), timestamp, id),
        )?;
        if updated == 0 && self.call_outcome(id)? != outcome {
            return Err(StorageError::InvalidState);
        }
        Ok(())
    }

    pub fn call_outcome(&self, id: &str) -> Result<CallOutcome, StorageError> {
        let value: String =
            self.connection
                .query_row("SELECT outcome FROM calls WHERE id=?1", [id], |row| {
                    row.get(0)
                })?;
        CallOutcome::parse(&value)
    }

    pub fn recording_finished(&mut self, name: &str) -> Result<(), StorageError> {
        validate_name(name)?;
        self.connection.execute("INSERT INTO recordings (name,format,outcome) VALUES (?1,'wav','completed') ON CONFLICT(name) DO UPDATE SET outcome='completed'", [name])?;
        Ok(())
    }

    fn recording_path(&self, name: &str) -> Result<PathBuf, StorageError> {
        validate_name(name)?;
        let path = self.recordings.join(format!("{name}.wav"));
        let metadata = std::fs::symlink_metadata(&path).map_err(recording_io_error)?;
        if metadata.file_type().is_symlink() {
            return Err(StorageError::InvalidName);
        }
        if !metadata.is_file() {
            return Err(StorageError::NotFound);
        }
        Ok(path)
    }

    pub fn recording(&self, name: &str) -> Result<rusterisk_api::StoredRecording, StorageError> {
        self.recording_path(name)?;
        Ok(rusterisk_api::StoredRecording {
            name: name.to_owned(),
            format: "wav".into(),
        })
    }

    pub fn open_recording(&self, name: &str) -> Result<StoredRecordingReader, StorageError> {
        StoredRecordingReader::open(&self.recording_path(name)?)
    }

    pub fn prepare_recording_copy(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<RecordingCopy, StorageError> {
        RecordingCopy::create(&self.recording_path(source)?, &self.recordings, destination)
    }

    pub fn delete_recording(&mut self, name: &str) -> Result<(), StorageError> {
        let path = self.recording_path(name)?;
        std::fs::remove_file(path).map_err(recording_io_error)?;
        self.connection
            .execute("DELETE FROM recordings WHERE name=?1", [name])?;
        Ok(())
    }

    pub fn recordings(&self) -> Result<Vec<rusterisk_api::StoredRecording>, StorageError> {
        let mut output = Vec::new();
        for (index, entry) in std::fs::read_dir(&self.recordings)?.enumerate() {
            let entry = entry?;
            if index >= MAX_DIRECTORY_ENTRIES {
                return Err(StorageError::Capacity);
            }
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|value| value == "wav")
            {
                let name = entry
                    .path()
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or(StorageError::InvalidName)?
                    .to_owned();
                validate_name(&name)?;
                output.push(rusterisk_api::StoredRecording {
                    name,
                    format: "wav".into(),
                });
            }
        }
        output.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(output)
    }
}

fn validate_name(name: &str) -> Result<(), StorageError> {
    rusterisk_api::ChannelId::try_from(name.to_owned()).map_err(|_| StorageError::InvalidName)?;
    Ok(())
}

fn recording_io_error(error: std::io::Error) -> StorageError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StorageError::NotFound
    } else {
        StorageError::Io(error)
    }
}

fn reserve_directory_entries(directory: &Path, additional: usize) -> Result<(), StorageError> {
    let mut count = additional;
    for entry in std::fs::read_dir(directory)? {
        entry?;
        count = count.checked_add(1).ok_or(StorageError::Capacity)?;
        if count > MAX_DIRECTORY_ENTRIES {
            return Err(StorageError::Capacity);
        }
    }
    Ok(())
}

fn header(rate: u32, bytes: u32) -> [u8; 44] {
    let mut header = [0u8; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + bytes).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&1u16.to_le_bytes());
    header[24..28].copy_from_slice(&rate.to_le_bytes());
    header[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
    header[32..34].copy_from_slice(&2u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&bytes.to_le_bytes());
    header
}

fn read_header(file: &mut File) -> Result<(u32, u32), StorageError> {
    let mut bytes = [0u8; 44];
    file.read_exact(&mut bytes)?;
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let size = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
    if !matches!(rate, 8000 | 12000 | 16000 | 24000 | 32000 | 44100 | 48000)
        || u64::from(size) > MAX_RECORDING
        || !size.is_multiple_of(2)
        || bytes != header(rate, size)
    {
        return Err(StorageError::InvalidWave);
    }
    Ok((rate, size))
}

pub struct Recorder {
    file: File,
    path: PathBuf,
    rate: u32,
    bytes: u32,
    prefix_bytes: u32,
    publication: Publication,
}

#[derive(Clone, Copy)]
enum Publication {
    Create,
    Replace,
}

impl Recorder {
    pub fn create(root: &Path, name: &str, rate: u32) -> Result<Self, StorageError> {
        Self::create_with_policy(root, name, rate, RecordingIfExists::Fail)
    }

    pub fn create_with_policy(
        root: &Path,
        name: &str,
        rate: u32,
        policy: RecordingIfExists,
    ) -> Result<Self, StorageError> {
        let mut preparation = RecordingPreparation::create(root, name, rate, policy)?;
        while !preparation.is_ready() {
            preparation.advance()?;
        }
        preparation.into_recorder()
    }

    fn stage(
        root: &Path,
        name: &str,
        rate: u32,
        policy: RecordingIfExists,
    ) -> Result<(Self, Option<WaveReader>), StorageError> {
        validate_name(name)?;
        if !matches!(rate, 8000 | 12000 | 16000 | 24000 | 32000 | 44100 | 48000) {
            return Err(StorageError::InvalidWave);
        }
        let publication = match policy {
            RecordingIfExists::Fail => Publication::Create,
            RecordingIfExists::Overwrite | RecordingIfExists::Append => Publication::Replace,
        };
        let directory = root.join("recordings");
        std::fs::create_dir_all(&directory)?;
        if std::fs::symlink_metadata(&directory)?
            .file_type()
            .is_symlink()
        {
            return Err(StorageError::InvalidName);
        }
        if matches!(publication, Publication::Create)
            && directory.join(format!("{name}.wav")).exists()
        {
            return Err(StorageError::Exists);
        }
        let (extension, additional) = match policy {
            RecordingIfExists::Fail => ("partial", 2),
            RecordingIfExists::Overwrite => ("replacement", 1),
            RecordingIfExists::Append => ("append", 1),
        };
        for staging in ["partial", "replacement", "append", "copying"] {
            if directory.join(format!("{name}.{staging}")).try_exists()? {
                return Err(StorageError::Exists);
            }
        }
        let completed = directory.join(format!("{name}.wav"));
        let source = if policy == RecordingIfExists::Append && completed.try_exists()? {
            if std::fs::symlink_metadata(&completed)?
                .file_type()
                .is_symlink()
            {
                return Err(StorageError::InvalidName);
            }
            let source = WaveReader::open(&completed)?;
            if source.rate != rate {
                return Err(StorageError::InvalidWave);
            }
            Some(source)
        } else {
            None
        };
        reserve_directory_entries(&directory, additional)?;
        let path = directory.join(format!("{name}.{extension}"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StorageError::Exists
                } else {
                    StorageError::Io(error)
                }
            })?;
        file.write_all(&header(
            rate,
            source.as_ref().map(|source| source.remaining).unwrap_or(0),
        ))?;
        file.sync_all()?;
        Ok((
            Self {
                file,
                path,
                rate,
                bytes: 0,
                prefix_bytes: source.as_ref().map(|source| source.remaining).unwrap_or(0),
                publication,
            },
            source,
        ))
    }

    pub fn write(&mut self, frame: Frame) -> Result<(), StorageError> {
        let frame = frame.resample(self.rate)?;
        let size = frame.samples().len() * 2;
        if u64::from(self.bytes) + size as u64 > MAX_RECORDING {
            return Err(StorageError::Capacity);
        }
        let mut bytes = [0u8; rusterisk_media::MAX_SAMPLES * 2];
        for (pair, sample) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(frame.samples()) {
            *pair = sample.to_le_bytes();
        }
        self.file.write_all(&bytes[..size])?;
        self.bytes += size as u32;
        Ok(())
    }

    pub fn finish(self) -> Result<PathBuf, StorageError> {
        self.finish_with_tail_trim(0)
    }

    pub fn finish_with_tail_trim(mut self, milliseconds: u64) -> Result<PathBuf, StorageError> {
        let maximum = self
            .bytes
            .checked_sub(self.prefix_bytes)
            .ok_or(StorageError::InvalidState)?;
        let samples = milliseconds.saturating_mul(u64::from(self.rate)) / 1000;
        let trim = samples.saturating_mul(2).min(u64::from(maximum));
        self.bytes -= trim as u32;
        self.file.set_len(u64::from(self.bytes) + 44)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.rate, self.bytes))?;
        self.file.sync_all()?;
        let destination = self.path.with_extension("wav");
        drop(self.file);
        match self.publication {
            Publication::Create => {
                std::fs::hard_link(&self.path, &destination)?;
                std::fs::remove_file(&self.path)?;
            }
            Publication::Replace => std::fs::rename(&self.path, &destination)?,
        }
        Ok(destination)
    }

    pub fn cancel(self) -> Result<(), StorageError> {
        drop(self.file);
        std::fs::remove_file(self.path)?;
        Ok(())
    }
}

fn recover(path: &Path) -> Result<bool, StorageError> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    if !(44..=MAX_RECORDING + 44).contains(&length) {
        return Err(StorageError::InvalidWave);
    }
    let (rate, declared) = read_header(&mut file)?;
    if path.extension().is_some_and(|value| value == "append") && length < u64::from(declared) + 44
    {
        drop(file);
        std::fs::remove_file(path)?;
        return Ok(false);
    }
    let bytes = (length - 44) / 2 * 2;
    file.set_len(bytes + 44)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(rate, bytes as u32))?;
    file.sync_all()?;
    let destination = path.with_extension("wav");
    if path
        .extension()
        .is_some_and(|value| value == "replacement" || value == "append")
    {
        drop(file);
        std::fs::rename(path, destination)?;
        return Ok(true);
    }
    match std::fs::hard_link(path, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut completed = File::open(&destination)?;
            if completed.metadata()?.len() != file.metadata()?.len() {
                return Err(StorageError::InvalidState);
            }
            file.seek(SeekFrom::Start(0))?;
            let mut left = [0u8; 8192];
            let mut right = [0u8; 8192];
            loop {
                let size = file.read(&mut left)?;
                if size == 0 {
                    break;
                }
                completed.read_exact(&mut right[..size])?;
                if left[..size] != right[..size] {
                    return Err(StorageError::InvalidState);
                }
            }
        }
        Err(error) => return Err(error.into()),
    }
    drop(file);
    std::fs::remove_file(path)?;
    Ok(true)
}

pub struct WaveReader {
    file: File,
    rate: u32,
    length: u32,
    remaining: u32,
}

impl WaveReader {
    pub fn sample_count(&self) -> u64 {
        u64::from(self.length) / 2
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn sample_position(&self) -> u64 {
        u64::from(self.length - self.remaining) / 2
    }

    pub fn metadata(&self) -> Result<std::fs::Metadata, StorageError> {
        self.file.metadata().map_err(StorageError::from)
    }

    pub fn seek_sample(&mut self, sample: u64) -> Result<u64, StorageError> {
        let sample = sample.min(u64::from(self.length) / 2);
        let bytes = u32::try_from(sample * 2).map_err(|_| StorageError::Capacity)?;
        self.file.seek(SeekFrom::Start(44 + u64::from(bytes)))?;
        self.remaining = self.length - bytes;
        Ok(sample)
    }

    pub fn open(path: &Path) -> Result<Self, StorageError> {
        Self::from_file(File::open(path)?)
    }

    pub fn from_file(mut file: File) -> Result<Self, StorageError> {
        file.seek(SeekFrom::Start(0))?;
        let (rate, remaining) = read_header(&mut file)?;
        if u64::from(remaining) + 44 != file.metadata()?.len() || !remaining.is_multiple_of(2) {
            return Err(StorageError::InvalidWave);
        }
        Ok(Self {
            file,
            rate,
            length: remaining,
            remaining,
        })
    }
    pub fn read_frame(&mut self) -> Result<Option<Frame>, StorageError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let size = (self.rate as usize / 50 * 2).min(self.remaining as usize);
        let mut bytes = [0u8; rusterisk_media::MAX_SAMPLES * 2];
        self.file.read_exact(&mut bytes[..size])?;
        let mut samples = [0i16; rusterisk_media::MAX_SAMPLES];
        for (sample, pair) in samples.iter_mut().zip(bytes[..size].as_chunks::<2>().0) {
            *sample = i16::from_le_bytes(*pair);
        }
        self.remaining -= size as u32;
        Ok(Some(Frame::new(&samples[..size / 2], self.rate)?))
    }
}
