use crate::{reserve_directory_entries, validate_name, StorageError, StoredRecordingReader};
use rusterisk_api::StoredRecording;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct RecordingCopy {
    source: Option<StoredRecordingReader>,
    file: File,
    path: PathBuf,
    name: String,
}

impl RecordingCopy {
    pub(crate) fn create(
        source: &Path,
        directory: &Path,
        name: &str,
    ) -> Result<Self, StorageError> {
        let source = StoredRecordingReader::open(source)?;
        validate_name(name)?;
        for extension in ["wav", "partial", "replacement", "append", "copying"] {
            match std::fs::symlink_metadata(directory.join(format!("{name}.{extension}"))) {
                Ok(metadata) => {
                    return Err(if metadata.file_type().is_symlink() {
                        StorageError::InvalidName
                    } else {
                        StorageError::Exists
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(StorageError::Io(error)),
            }
        }
        reserve_directory_entries(directory, 2)?;
        let path = directory.join(format!("{name}.copying"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StorageError::Exists
                } else {
                    StorageError::Io(error)
                }
            })?;
        Ok(Self {
            source: Some(source),
            file,
            path,
            name: name.to_owned(),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.source.is_none()
    }

    pub fn advance(&mut self) -> Result<(), StorageError> {
        if let Some(source) = &mut self.source {
            match source.read_chunk()? {
                Some(chunk) => self.file.write_all(chunk.as_ref())?,
                None => self.source = None,
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<StoredRecording, StorageError> {
        if !self.is_ready() {
            return Err(StorageError::InvalidState);
        }
        self.file.sync_all()?;
        drop(self.file);
        std::fs::hard_link(&self.path, self.path.with_extension("wav"))?;
        std::fs::remove_file(self.path)?;
        Ok(StoredRecording {
            name: self.name,
            format: "wav".into(),
        })
    }

    pub fn cancel(self) -> Result<(), StorageError> {
        drop(self.source);
        drop(self.file);
        std::fs::remove_file(self.path)?;
        Ok(())
    }
}
