use super::MusicSelection;
use rusterisk_storage::{StorageError, WaveReader};
use std::task::Poll;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MusicCursor {
    pub(super) track: usize,
    pub(super) sample: u64,
    pub(super) offset: u16,
}

impl MusicCursor {
    pub(in crate::storage) const fn at(track: usize, sample: u64) -> Self {
        Self {
            track,
            sample,
            offset: 0,
        }
    }

    pub(in crate::storage) const fn with_sample(self, sample: u64) -> Self {
        Self {
            sample,
            offset: 0,
            ..self
        }
    }
}

impl MusicSelection {
    fn output_samples(&self, track: usize) -> Result<u64, StorageError> {
        let track = self
            .class
            .tracks
            .get(track)
            .ok_or(StorageError::InvalidState)?;
        let period = u64::from(track.rate / 50);
        let complete = (track.samples / period)
            .checked_mul(960)
            .ok_or(StorageError::Capacity)?;
        let tail = (track.samples % period)
            .checked_mul(48000)
            .ok_or(StorageError::Capacity)?
            / u64::from(track.rate);
        complete.checked_add(tail).ok_or(StorageError::Capacity)
    }

    fn sample_count(&self) -> Result<u64, StorageError> {
        (0..self.class.tracks.len()).try_fold(0u64, |sum, index| {
            sum.checked_add(self.output_samples(index)?)
                .ok_or(StorageError::Capacity)
        })
    }

    fn cursor_position(&self, cursor: MusicCursor) -> Result<u64, StorageError> {
        let track = self
            .class
            .tracks
            .get(cursor.track)
            .ok_or(StorageError::InvalidState)?;
        let prefix = (0..cursor.track).try_fold(0u64, |sum, index| {
            sum.checked_add(self.output_samples(index)?)
                .ok_or(StorageError::Capacity)
        })?;
        let position = (cursor.sample / u64::from(track.rate / 50))
            .checked_mul(960)
            .and_then(|samples| samples.checked_add(u64::from(cursor.offset)))
            .ok_or(StorageError::Capacity)?;
        prefix.checked_add(position).ok_or(StorageError::Capacity)
    }
}

struct BufferedMusic {
    frame: rusterisk_media::Frame,
    sample: u64,
    offset: usize,
}

pub(in crate::storage) struct MusicReader {
    selection: MusicSelection,
    reader: WaveReader,
    track: usize,
    cursor: MusicCursor,
    skip: usize,
}

pub(in crate::storage) struct MusicBuffer {
    buffered: Option<BufferedMusic>,
    samples: [i16; 960],
    length: usize,
}

impl MusicBuffer {
    pub(in crate::storage) const fn new() -> Self {
        Self {
            buffered: None,
            samples: [0; 960],
            length: 0,
        }
    }

    pub(in crate::storage) const fn is_preparing(&self) -> bool {
        self.length > 0
    }

    fn clear(&mut self) {
        self.buffered = None;
        self.length = 0;
    }
}

impl MusicReader {
    pub(in crate::storage) fn new(
        selection: MusicSelection,
        cursor: Option<MusicCursor>,
    ) -> Result<Self, StorageError> {
        let cursor = cursor.unwrap_or_else(|| MusicCursor::at(0, 0));
        let (reader, track) = selection.open(Some(cursor))?;
        Ok(Self {
            selection,
            reader,
            track,
            cursor,
            skip: usize::from(cursor.offset),
        })
    }

    pub(in crate::storage) fn sample_count(&self) -> Result<u64, StorageError> {
        self.selection.sample_count()
    }

    pub(in crate::storage) fn sample_position(&self) -> Result<u64, StorageError> {
        self.selection.cursor_position(self.cursor)
    }

    pub(in crate::storage) const fn cursor(&self) -> MusicCursor {
        self.cursor
    }

    pub(in crate::storage) fn seek_sample(
        &mut self,
        sample: u64,
        buffer: &mut MusicBuffer,
    ) -> Result<(), StorageError> {
        let mut remaining = sample.min(self.sample_count()?);
        let mut cursor = MusicCursor::at(0, 0);
        for (index, track) in self.selection.class.tracks.iter().enumerate() {
            let length = self.selection.output_samples(index)?;
            if remaining < length {
                cursor = MusicCursor {
                    track: index,
                    sample: (remaining / 960) * u64::from(track.rate / 50),
                    offset: (remaining % 960) as u16,
                };
                break;
            }
            remaining -= length;
        }
        let (reader, track) = self.selection.open(Some(cursor))?;
        self.reader = reader;
        self.track = track;
        self.cursor = cursor;
        self.skip = usize::from(cursor.offset);
        buffer.clear();
        Ok(())
    }

    fn advance(&mut self) -> Result<(), StorageError> {
        let next = (self.track + 1) % self.selection.class.tracks.len();
        if next == self.track {
            self.reader.seek_sample(0)?;
        } else {
            let (reader, track) = self.selection.open(Some(MusicCursor::at(next, 0)))?;
            self.reader = reader;
            self.track = track;
        }
        self.cursor = MusicCursor::at(next, 0);
        self.skip = 0;
        Ok(())
    }

    pub(in crate::storage) fn read_frame(
        &mut self,
        buffer: &mut MusicBuffer,
    ) -> Result<Poll<Option<rusterisk_media::Frame>>, StorageError> {
        for _ in 0..64 {
            if buffer.buffered.is_none() {
                let sample = self.reader.sample_position();
                let Some(frame) = self.reader.read_frame()? else {
                    self.advance()?;
                    continue;
                };
                let frame = frame.resample(48000)?;
                let offset = std::mem::take(&mut self.skip);
                if offset >= frame.samples().len() {
                    return Err(StorageError::InvalidState);
                }
                buffer.buffered = Some(BufferedMusic {
                    frame,
                    sample,
                    offset,
                });
            }
            let buffered = buffer.buffered.as_mut().ok_or(StorageError::InvalidState)?;
            let count = (960 - buffer.length).min(buffered.frame.samples().len() - buffered.offset);
            buffer.samples[buffer.length..buffer.length + count].copy_from_slice(
                &buffered.frame.samples()[buffered.offset..buffered.offset + count],
            );
            buffer.length += count;
            buffered.offset += count;
            if buffered.offset == buffered.frame.samples().len() {
                buffer.buffered = None;
                let sample = self.reader.sample_position();
                let track = self
                    .selection
                    .class
                    .tracks
                    .get(self.track)
                    .ok_or(StorageError::InvalidState)?;
                self.cursor = if sample == track.samples {
                    MusicCursor::at((self.track + 1) % self.selection.class.tracks.len(), 0)
                } else {
                    MusicCursor::at(self.track, sample)
                };
            } else {
                self.cursor = MusicCursor {
                    track: self.track,
                    sample: buffered.sample,
                    offset: u16::try_from(buffered.offset).map_err(|_| StorageError::Capacity)?,
                };
            }
            if buffer.length == 960 {
                let frame = rusterisk_media::Frame::new(&buffer.samples, 48000)?;
                buffer.length = 0;
                return Ok(Poll::Ready(Some(frame)));
            }
        }
        Ok(Poll::Pending)
    }
}
