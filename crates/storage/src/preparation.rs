use crate::{Recorder, StorageError, WaveReader};
use rusterisk_api::RecordingIfExists;
use rusterisk_media::Frame;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct RecordingPreparation {
    recorder: Recorder,
    source: Option<WaveReader>,
}

impl RecordingPreparation {
    pub fn create(
        root: &Path,
        name: &str,
        rate: u32,
        policy: RecordingIfExists,
    ) -> Result<Self, StorageError> {
        let (recorder, source) = Recorder::stage(root, name, rate, policy)?;
        Ok(Self { recorder, source })
    }

    pub fn is_ready(&self) -> bool {
        self.source.is_none()
    }

    pub fn advance(&mut self) -> Result<(), StorageError> {
        if let Some(source) = &mut self.source {
            let mut bytes = [0; 8192];
            let size = bytes.len().min(source.remaining as usize);
            source.file.read_exact(&mut bytes[..size])?;
            self.recorder.file.write_all(&bytes[..size])?;
            self.recorder.bytes += size as u32;
            source.remaining -= size as u32;
            if source.remaining == 0 {
                self.recorder.file.sync_all()?;
                self.source = None;
            }
        }
        Ok(())
    }

    pub fn write(&mut self, frame: Frame) -> Result<(), StorageError> {
        if !self.is_ready() {
            return Err(StorageError::InvalidState);
        }
        self.recorder.write(frame)
    }

    pub fn finish(self) -> Result<PathBuf, StorageError> {
        self.into_recorder()?.finish()
    }

    pub fn finish_with_tail_trim(self, milliseconds: u64) -> Result<PathBuf, StorageError> {
        self.into_recorder()?.finish_with_tail_trim(milliseconds)
    }

    pub fn cancel(self) -> Result<(), StorageError> {
        drop(self.source);
        self.recorder.cancel()
    }

    pub(crate) fn into_recorder(self) -> Result<Recorder, StorageError> {
        if !self.is_ready() {
            return Err(StorageError::InvalidState);
        }
        Ok(self.recorder)
    }
}
