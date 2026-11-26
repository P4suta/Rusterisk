use crate::{
    journal::Operation, reserve_directory_entries, validate_name, StorageError, Store,
    StoredRecordingReader,
};
use rusterisk_api::StoredRecording;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub struct RecordingCopy {
    operation: Operation,
    source: Option<StoredRecordingReader>,
    file: File,
    path: PathBuf,
    name: String,
}

impl RecordingCopy {
    pub(crate) fn create(
        store: &Store,
        source_name: &str,
        name: &str,
    ) -> Result<Self, StorageError> {
        let source = store.open_recording(source_name)?;
        let directory = &store.recordings;
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
        let operation = Operation::start(store, name, "copy", Some(source_name))?;
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StorageError::Exists
                } else {
                    StorageError::Io(error)
                }
            });
        let file = match opened {
            Ok(file) => file,
            Err(error) => {
                operation.forget()?;
                return Err(error);
            }
        };
        Ok(Self {
            operation,
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
            self.cancel()?;
            return Err(StorageError::InvalidState);
        }
        self.file.sync_all()?;
        let bytes =
            u32::try_from(self.file.metadata()?.len()).map_err(|_| StorageError::Capacity)?;
        self.operation.publishing(None, bytes)?;
        drop(self.file);
        let publication = std::fs::hard_link(&self.path, self.path.with_extension("wav"));
        let cleanup = std::fs::remove_file(&self.path);
        match (publication, cleanup) {
            (Ok(()), Ok(())) => {}
            (Err(operation), Err(cleanup)) => {
                return Err(StorageError::PublicationCleanup { operation, cleanup });
            }
            (Err(error), Ok(())) => {
                self.operation.forget()?;
                return Err(StorageError::Io(error));
            }
            (Ok(()), Err(error)) => return Err(StorageError::Io(error)),
        }
        #[cfg(unix)]
        File::open(self.path.parent().ok_or(StorageError::InvalidState)?)?.sync_all()?;
        self.operation.completed()?;
        Ok(StoredRecording {
            name: self.name,
            format: "wav".into(),
        })
    }

    pub fn cancel(self) -> Result<(), StorageError> {
        self.operation.cancelling()?;
        drop(self.source);
        drop(self.file);
        std::fs::remove_file(&self.path)?;
        #[cfg(unix)]
        File::open(self.path.parent().ok_or(StorageError::InvalidState)?)?.sync_all()?;
        self.operation.forget()
    }
}
