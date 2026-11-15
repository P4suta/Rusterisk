use crate::{recording_io_error, StorageError, MAX_RECORDING};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub struct RecordingChunk {
    bytes: [u8; 8192],
    length: usize,
}

impl AsRef<[u8]> for RecordingChunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

pub struct StoredRecordingReader {
    file: File,
    length: u64,
    remaining: u64,
}

impl StoredRecordingReader {
    pub(crate) fn open(path: &Path) -> Result<Self, StorageError> {
        let file = File::open(path).map_err(recording_io_error)?;
        let length = file.metadata()?.len();
        if length > MAX_RECORDING + 44 {
            return Err(StorageError::Capacity);
        }
        Ok(Self {
            file,
            length,
            remaining: length,
        })
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub fn read_chunk(&mut self) -> Result<Option<RecordingChunk>, StorageError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let length = self.remaining.min(8192) as usize;
        let mut chunk = RecordingChunk {
            bytes: [0; 8192],
            length,
        };
        self.file.read_exact(&mut chunk.bytes[..length])?;
        self.remaining -= length as u64;
        Ok(Some(chunk))
    }
}
