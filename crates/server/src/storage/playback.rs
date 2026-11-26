use super::music::{MusicBuffer, MusicReader};
#[cfg(test)]
use super::PreparedMusic;
use super::{receive, MusicCursor, MusicSelection, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, SoundId};
use rusterisk_media::Frame;
use rusterisk_storage::{StorageError, WaveReader};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub enum PlaybackMedia {
    Recording(ChannelId),
    Sound(SoundId),
    LocalizedSound { name: SoundId, language: ChannelId },
}

pub(super) enum Input {
    Finite(PlaybackMedia),
    Music {
        selection: MusicSelection,
        cursor: Option<MusicCursor>,
    },
}

pub(super) struct Packet {
    epoch: u64,
    frame: Option<Frame>,
    music: Option<MusicCursor>,
}

pub(super) struct Seek {
    epoch: u64,
    sample: u64,
}

pub(super) struct Metadata {
    rate: u32,
    samples: u64,
    music: Option<MusicCursor>,
    position: u64,
}

enum State {
    Active,
    Ended,
    Failed,
}

struct Lease {
    lifetime: Option<oneshot::Sender<()>>,
    wake: mpsc::Sender<()>,
    stop: tokio::sync::watch::Receiver<bool>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        drop(self.lifetime.take());
        super::notify(&self.wake);
    }
}

pub struct PlaybackSource {
    frames: mpsc::Receiver<Packet>,
    seeks: mpsc::Sender<Seek>,
    completed: oneshot::Receiver<Result<(), StorageError>>,
    state: State,
    lease: Lease,
    metadata: Metadata,
    epoch: u64,
    position: u64,
    music: Option<MusicCursor>,
}

impl PlaybackSource {
    pub fn rate(&self) -> u32 {
        self.metadata.rate
    }

    pub fn sample_count(&self) -> u64 {
        self.metadata.samples
    }

    pub fn sample_position(&self) -> u64 {
        self.position
    }

    pub(crate) fn music_cursor(&self) -> Option<MusicCursor> {
        self.music
    }

    pub fn seek_sample(&mut self, sample: u64) -> Result<(), WorkerError> {
        if !matches!(self.state, State::Active) || *self.lease.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        let epoch = self.epoch.checked_add(1).ok_or(WorkerError::Capacity)?;
        let sample = sample.min(self.metadata.samples);
        self.seeks
            .try_send(Seek { epoch, sample })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Capacity,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
            })?;
        self.epoch = epoch;
        self.position = sample;
        self.music = self.music.map(|cursor| cursor.with_sample(sample));
        super::notify(&self.lease.wake);
        Ok(())
    }

    fn packet(&mut self, packet: Packet) -> Option<Option<Frame>> {
        super::notify(&self.lease.wake);
        if packet.epoch != self.epoch {
            return None;
        }
        self.music = packet.music;
        match packet.frame {
            Some(frame) => {
                self.position += frame.samples().len() as u64;
                Some(Some(frame))
            }
            None => {
                self.state = State::Ended;
                Some(None)
            }
        }
    }

    fn complete(&mut self, result: Result<(), WorkerError>) -> Result<Option<Frame>, WorkerError> {
        self.state = if result.is_ok() {
            State::Ended
        } else {
            State::Failed
        };
        result.map(|()| None)
    }

    pub fn try_read(&mut self) -> Result<Poll<Option<Frame>>, WorkerError> {
        match self.state {
            State::Ended => return Ok(Poll::Ready(None)),
            State::Failed => return Err(WorkerError::Closed),
            State::Active => {}
        }
        for _ in 0..3 {
            match self.frames.try_recv() {
                Ok(packet) => {
                    if let Some(frame) = self.packet(packet) {
                        return Ok(Poll::Ready(frame));
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => return Ok(Poll::Pending),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    let result = match self.completed.try_recv() {
                        Ok(result) => result.map_err(WorkerError::Storage),
                        Err(oneshot::error::TryRecvError::Empty) => return Ok(Poll::Pending),
                        Err(oneshot::error::TryRecvError::Closed) => Err(WorkerError::Closed),
                    };
                    return self.complete(result).map(Poll::Ready);
                }
            }
        }
        Ok(Poll::Pending)
    }

    pub async fn read(&mut self) -> Result<Option<Frame>, WorkerError> {
        match self.try_read()? {
            Poll::Ready(frame) => return Ok(frame),
            Poll::Pending => {}
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self.frames.recv().await {
                    Some(packet) => {
                        if let Some(frame) = self.packet(packet) {
                            return Ok(frame);
                        }
                    }
                    None => {
                        let result = (&mut self.completed)
                            .await
                            .map_err(|_| WorkerError::Closed)?
                            .map_err(WorkerError::Storage);
                        return self.complete(result);
                    }
                }
            }
        })
        .await
        .map_err(|_| WorkerError::Timeout)?
    }
}

impl Drop for PlaybackSource {
    fn drop(&mut self) {
        self.frames.close();
    }
}

impl StoragePort {
    pub async fn play(&self, media: PlaybackMedia) -> Result<PlaybackSource, WorkerError> {
        self.play_input(Input::Finite(media)).await
    }

    #[cfg(test)]
    pub(crate) async fn play_music(
        &self,
        catalog: &PreparedMusic,
        class: &str,
        cursor: Option<MusicCursor>,
    ) -> Result<PlaybackSource, WorkerError> {
        self.play_selected_music(catalog.select(class).ok_or(WorkerError::Invalid)?, cursor)
            .await
    }

    pub(crate) async fn play_selected_music(
        &self,
        selection: MusicSelection,
        cursor: Option<MusicCursor>,
    ) -> Result<PlaybackSource, WorkerError> {
        self.play_input(Input::Music { selection, cursor }).await
    }

    async fn play_input(&self, media: Input) -> Result<PlaybackSource, WorkerError> {
        let (output, frames) = mpsc::channel(3);
        let (seek, seeks) = mpsc::channel(8);
        let (completed, completion) = oneshot::channel();
        let (lifetime, owner) = oneshot::channel();
        let lease = Lease {
            lifetime: Some(lifetime),
            wake: self.wake.clone(),
            stop: self.stop.clone(),
        };
        let (reply, response) = oneshot::channel();
        self.send(super::Command::Play {
            media,
            output,
            seeks,
            lifetime: owner,
            completed,
            reply,
        })?;
        let metadata = receive(response).await?;
        let position = metadata.position;
        Ok(PlaybackSource {
            frames,
            seeks: seek,
            completed: completion,
            state: State::Active,
            lease,
            music: metadata.music,
            metadata,
            epoch: 0,
            position,
        })
    }
}

enum Reader {
    Finite(WaveReader),
    Music(MusicReader),
}

impl Reader {
    fn metadata(&self) -> Result<Metadata, StorageError> {
        match self {
            Self::Finite(reader) => Ok(Metadata {
                rate: reader.rate(),
                samples: reader.sample_count(),
                position: reader.sample_position(),
                music: None,
            }),
            Self::Music(reader) => Ok(Metadata {
                rate: 48000,
                samples: reader.sample_count()?,
                position: reader.sample_position()?,
                music: Some(reader.cursor()),
            }),
        }
    }

    fn cursor(&self) -> Option<MusicCursor> {
        match self {
            Self::Finite(_) => None,
            Self::Music(reader) => Some(reader.cursor()),
        }
    }

    fn seek_sample(&mut self, sample: u64, buffer: &mut MusicBuffer) -> Result<(), StorageError> {
        match self {
            Self::Finite(reader) => reader.seek_sample(sample).map(|_| ()),
            Self::Music(reader) => reader.seek_sample(sample, buffer),
        }
    }

    fn read_frame(
        &mut self,
        buffer: &mut MusicBuffer,
    ) -> Result<Poll<Option<Frame>>, StorageError> {
        match self {
            Self::Finite(reader) => reader.read_frame().map(Poll::Ready),
            Self::Music(reader) => reader.read_frame(buffer),
        }
    }
}

struct Entry {
    reader: Reader,
    buffer: MusicBuffer,
    output: mpsc::Sender<Packet>,
    seeks: mpsc::Receiver<Seek>,
    epoch: u64,
    ended: bool,
    lifetime: oneshot::Receiver<()>,
    completed: oneshot::Sender<Result<(), StorageError>>,
}

impl Entry {
    fn seek(&mut self) -> Result<(), StorageError> {
        for _ in 0..8 {
            let command = match self.seeks.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break
                }
            };
            if command.epoch <= self.epoch {
                return Err(StorageError::InvalidState);
            }
            self.reader.seek_sample(command.sample, &mut self.buffer)?;
            self.epoch = command.epoch;
            self.ended = false;
        }
        Ok(())
    }
}

pub(super) struct PlaybackSet {
    root: PathBuf,
    generation: u64,
    entries: BTreeMap<u64, Entry>,
}

impl PlaybackSet {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn is_preparing(&self) -> bool {
        self.entries
            .values()
            .any(|entry| !entry.ended && entry.output.capacity() > 0 && entry.buffer.is_preparing())
    }

    pub fn begin(
        &mut self,
        media: Input,
        output: mpsc::Sender<Packet>,
        seeks: mpsc::Receiver<Seek>,
        lifetime: oneshot::Receiver<()>,
        completed: oneshot::Sender<Result<(), StorageError>>,
    ) -> Result<Metadata, StorageError> {
        if self.entries.len() >= 64 {
            return Err(StorageError::Capacity);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::Capacity)?;
        let reader = match media {
            Input::Music { selection, cursor } => {
                Reader::Music(MusicReader::new(selection, cursor)?)
            }
            Input::Finite(PlaybackMedia::Sound(name)) => {
                Reader::Finite(super::sounds::open(&self.root, &name, "en")?)
            }
            Input::Finite(PlaybackMedia::LocalizedSound { name, language }) => {
                Reader::Finite(super::sounds::open(&self.root, &name, language.as_str())?)
            }
            Input::Finite(PlaybackMedia::Recording(name)) => {
                let directory = self.root.join("recordings");
                if !std::fs::symlink_metadata(&directory)?.is_dir() {
                    return Err(StorageError::InvalidName);
                }
                let path = directory.join(format!("{name}.wav"));
                if !std::fs::symlink_metadata(&path)?.is_file() {
                    return Err(StorageError::InvalidName);
                }
                Reader::Finite(WaveReader::open(&path)?)
            }
        };
        let metadata = reader.metadata()?;
        self.entries.insert(
            generation,
            Entry {
                reader,
                buffer: MusicBuffer::new(),
                output,
                seeks,
                epoch: 0,
                ended: false,
                lifetime,
                completed,
            },
        );
        self.generation = generation;
        Ok(metadata)
    }

    pub fn pump(&mut self) {
        let mut finished = Vec::with_capacity(self.entries.len());
        for (key, entry) in &mut self.entries {
            match entry.lifetime.try_recv() {
                Ok(()) | Err(oneshot::error::TryRecvError::Closed) => {
                    finished.push((*key, Ok(())));
                    continue;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
            if let Err(error) = entry.seek() {
                finished.push((*key, Err(error)));
                continue;
            }
            if entry.ended {
                continue;
            }
            for _ in 0..entry.output.capacity().min(3) {
                let result = match entry.reader.read_frame(&mut entry.buffer) {
                    Ok(Poll::Pending) => break,
                    Ok(Poll::Ready(frame)) => {
                        let ended = frame.is_none();
                        match entry.output.try_send(Packet {
                            epoch: entry.epoch,
                            frame,
                            music: entry.reader.cursor(),
                        }) {
                            Ok(()) if ended => {
                                entry.ended = true;
                                break;
                            }
                            Ok(()) => continue,
                            Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
                            Err(mpsc::error::TrySendError::Full(_)) => Err(StorageError::Capacity),
                        }
                    }
                    Err(error) => {
                        tracing::warn!(playback = key, %error, "playback read failed");
                        Err(error)
                    }
                };
                finished.push((*key, result));
                break;
            }
        }
        for (key, result) in finished {
            if let Some(entry) = self.entries.remove(&key) {
                if entry.completed.send(result).is_err() {
                    tracing::debug!(playback = key, "playback receiver disconnected");
                }
            }
        }
    }
}

#[cfg(test)]
mod seek_tests {
    use super::{Metadata, Packet, PlaybackMedia, StoragePort, WorkerError};
    use rusterisk_storage::StorageError;
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    use tokio::sync::{mpsc, watch};

    #[tokio::test]
    async fn cancelling_an_open_wakes_the_worker_after_its_previous_wake() -> Result<(), WorkerError>
    {
        let (commands, mut requests) = mpsc::channel(1);
        let (stored, _stored) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let (wake, mut wakes) = mpsc::channel(1);
        let port = StoragePort {
            recording: mpsc::channel(1).0,
            commands,
            stored,
            stop,
            wake,
        };
        let id = rusterisk_api::ChannelId::try_from("cancelled".to_owned())
            .map_err(|_| WorkerError::Invalid)?;
        {
            let mut opening = std::pin::pin!(port.play(PlaybackMedia::Recording(id)));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(opening.as_mut().poll(&mut context), Poll::Pending));
            wakes.try_recv().map_err(|_| WorkerError::Invalid)?;
        }
        let super::super::Command::Play {
            mut lifetime,
            output,
            ..
        } = requests.try_recv().map_err(|_| WorkerError::Invalid)?
        else {
            return Err(WorkerError::Invalid);
        };
        assert!(output.is_closed());
        assert!(matches!(
            lifetime.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(wakes.try_recv().is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn seek_admission_preserves_state_and_stale_packets_cannot_end_the_current_epoch(
    ) -> Result<(), WorkerError> {
        let (commands, mut requests) = mpsc::channel(1);
        let (stored, _stored) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let (wake, _wakes) = mpsc::channel(1);
        let port = StoragePort {
            recording: mpsc::channel(1).0,
            commands,
            stored,
            stop,
            wake,
        };
        let id = rusterisk_api::ChannelId::try_from("bounded".to_owned())
            .map_err(|_| WorkerError::Invalid)?;
        let mut opening = std::pin::pin!(port.play(PlaybackMedia::Recording(id)));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(opening.as_mut().poll(&mut context), Poll::Pending));
        let super::super::Command::Play {
            output,
            mut seeks,
            completed,
            reply,
            lifetime: _lifetime,
            ..
        } = requests.try_recv().map_err(|_| WorkerError::Invalid)?
        else {
            return Err(WorkerError::Invalid);
        };
        reply
            .send(Ok(Metadata {
                rate: 8000,
                samples: 1600,
                music: None,
                position: 0,
            }))
            .map_err(|_| WorkerError::Invalid)?;
        let mut source = opening.await?;
        for sample in 0..8 {
            source.seek_sample(sample)?;
        }
        assert_eq!(source.sample_position(), 7);
        assert!(matches!(source.seek_sample(99), Err(WorkerError::Capacity)));
        assert_eq!(source.sample_position(), 7);
        assert_eq!(source.epoch, 8);
        for epoch in 1..=8 {
            let command = seeks.try_recv().map_err(|_| WorkerError::Invalid)?;
            assert_eq!(command.epoch, epoch);
            assert_eq!(command.sample, epoch - 1);
        }
        for (epoch, frame) in [
            (0, None),
            (7, None),
            (
                0,
                Some(rusterisk_media::Frame::new(&[1; 160], 8000).map_err(StorageError::from)?),
            ),
        ] {
            output
                .try_send(Packet {
                    epoch,
                    frame,
                    music: None,
                })
                .map_err(|_| WorkerError::Invalid)?;
        }
        assert!(matches!(source.try_read()?, Poll::Pending));
        output
            .try_send(Packet {
                epoch: 8,
                frame: Some(
                    rusterisk_media::Frame::new(&[3200; 160], 8000).map_err(StorageError::from)?,
                ),
                music: None,
            })
            .map_err(|_| WorkerError::Invalid)?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[3200; 160]
        );
        assert_eq!(source.sample_position(), 167);
        source.epoch = u64::MAX;
        assert!(matches!(source.seek_sample(0), Err(WorkerError::Capacity)));
        assert_eq!(source.sample_position(), 167);
        assert!(seeks.try_recv().is_err());
        completed
            .send(Err(StorageError::InvalidWave))
            .map_err(|_| WorkerError::Invalid)?;
        drop(output);
        assert!(matches!(
            source.read().await,
            Err(WorkerError::Storage(StorageError::InvalidWave))
        ));
        assert!(matches!(source.seek_sample(0), Err(WorkerError::Closed)));
        Ok(())
    }
}
