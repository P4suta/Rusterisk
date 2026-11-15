use super::{receive, StoragePort, WorkerError};
use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_storage::{StorageError, WaveReader};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub enum PlaybackMedia {
    Recording(ChannelId),
    Sound(ChannelId),
}

enum State {
    Active,
    Ended,
    Failed,
}

pub struct PlaybackSource {
    frames: mpsc::Receiver<Frame>,
    completed: oneshot::Receiver<Result<(), StorageError>>,
    lifetime: Option<oneshot::Sender<()>>,
    state: State,
    port: StoragePort,
}

impl PlaybackSource {
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
        match self.frames.try_recv() {
            Ok(frame) => {
                self.port.wake();
                Ok(Poll::Ready(Some(frame)))
            }
            Err(mpsc::error::TryRecvError::Empty) => Ok(Poll::Pending),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                let result = match self.completed.try_recv() {
                    Ok(result) => result.map_err(WorkerError::Storage),
                    Err(oneshot::error::TryRecvError::Empty) => return Ok(Poll::Pending),
                    Err(oneshot::error::TryRecvError::Closed) => Err(WorkerError::Closed),
                };
                self.complete(result).map(Poll::Ready)
            }
        }
    }

    pub async fn read(&mut self) -> Result<Option<Frame>, WorkerError> {
        match self.try_read()? {
            Poll::Ready(frame) => return Ok(frame),
            Poll::Pending => {}
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
            .await
            .map_err(|_| WorkerError::Timeout)?;
        self.port.wake();
        match frame {
            Some(frame) => Ok(Some(frame)),
            None => {
                let result = tokio::time::timeout(Duration::from_secs(5), &mut self.completed)
                    .await
                    .map_err(|_| WorkerError::Timeout)?
                    .map_err(|_| WorkerError::Closed)?
                    .map_err(WorkerError::Storage);
                self.complete(result)
            }
        }
    }
}

impl Drop for PlaybackSource {
    fn drop(&mut self) {
        self.frames.close();
        drop(self.lifetime.take());
        self.port.wake();
    }
}

impl StoragePort {
    pub async fn play(&self, media: PlaybackMedia) -> Result<PlaybackSource, WorkerError> {
        let (output, frames) = mpsc::channel(3);
        let (completed, completion) = oneshot::channel();
        let (lifetime, owner) = oneshot::channel();
        let source = PlaybackSource {
            frames,
            completed: completion,
            lifetime: Some(lifetime),
            state: State::Active,
            port: self.clone(),
        };
        let (reply, response) = oneshot::channel();
        self.send(super::Command::Play {
            media,
            output,
            lifetime: owner,
            completed,
            reply,
        })?;
        receive(response).await?;
        Ok(source)
    }
}

struct Entry {
    reader: WaveReader,
    output: mpsc::Sender<Frame>,
    lifetime: oneshot::Receiver<()>,
    completed: oneshot::Sender<Result<(), StorageError>>,
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

    pub fn begin(
        &mut self,
        media: PlaybackMedia,
        output: mpsc::Sender<Frame>,
        lifetime: oneshot::Receiver<()>,
        completed: oneshot::Sender<Result<(), StorageError>>,
    ) -> Result<(), StorageError> {
        if self.entries.len() >= 64 {
            return Err(StorageError::Capacity);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::Capacity)?;
        let (directory, name) = match media {
            PlaybackMedia::Recording(name) => ("recordings", name),
            PlaybackMedia::Sound(name) => ("sounds", name),
        };
        let directory = self.root.join(directory);
        if !std::fs::symlink_metadata(&directory)?.is_dir() {
            return Err(StorageError::InvalidName);
        }
        let path = directory.join(format!("{name}.wav"));
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(StorageError::InvalidName);
        }
        let reader = WaveReader::open(&path)?;
        self.entries.insert(
            generation,
            Entry {
                reader,
                output,
                lifetime,
                completed,
            },
        );
        self.generation = generation;
        Ok(())
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
            for _ in 0..entry.output.capacity().min(3) {
                let result = match entry.reader.read_frame() {
                    Ok(Some(frame)) => match entry.output.try_send(frame) {
                        Ok(()) => continue,
                        Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
                        Err(mpsc::error::TrySendError::Full(_)) => Err(StorageError::Capacity),
                    },
                    Ok(None) => Ok(()),
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
