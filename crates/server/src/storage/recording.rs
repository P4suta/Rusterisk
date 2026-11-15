use super::{receive, Command, StoragePort, WorkerError};
use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_storage::{Recorder, StorageError, Store};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RecordingKey {
    name: ChannelId,
    generation: u64,
}

enum SinkState {
    Active(mpsc::Sender<Frame>),
    Failed(SinkFailure),
    Released,
}

#[derive(Clone, Copy)]
enum SinkFailure {
    Capacity,
    Closed,
}

struct Lease {
    state: SinkState,
    lifetime: Option<oneshot::Sender<()>>,
    port: StoragePort,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.state = SinkState::Released;
        drop(self.lifetime.take());
        self.port.wake();
    }
}

pub struct RecordingSink {
    key: RecordingKey,
    lease: Lease,
}

impl RecordingSink {
    pub fn try_write(&mut self, frame: Frame) -> Result<(), WorkerError> {
        if *self.lease.port.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        let result = match &self.lease.state {
            SinkState::Active(sender) => sender.try_send(frame).map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => SinkFailure::Capacity,
                mpsc::error::TrySendError::Closed(_) => SinkFailure::Closed,
            }),
            SinkState::Failed(failure) => Err(*failure),
            SinkState::Released => Err(SinkFailure::Closed),
        };
        self.lease.port.wake();
        match result {
            Ok(()) => Ok(()),
            Err(failure) => {
                self.lease.state = SinkState::Failed(failure);
                Err(match failure {
                    SinkFailure::Capacity => WorkerError::Capacity,
                    SinkFailure::Closed => WorkerError::Closed,
                })
            }
        }
    }

    pub async fn finish(mut self) -> Result<(), WorkerError> {
        if matches!(self.lease.state, SinkState::Failed(SinkFailure::Capacity)) {
            return Err(WorkerError::Capacity);
        }
        let (reply, response) = oneshot::channel();
        self.lease.port.send(Command::FinishRecording {
            key: self.key.clone(),
            reply,
        })?;
        let result = receive(response).await;
        self.lease.state = SinkState::Released;
        result
    }

    pub async fn cancel(mut self) -> Result<(), WorkerError> {
        let (reply, response) = oneshot::channel();
        self.lease.port.send(Command::CancelRecording {
            key: self.key.clone(),
            reply,
        })?;
        let result = receive(response).await;
        self.lease.state = SinkState::Released;
        result
    }
}

impl StoragePort {
    pub async fn record(&self, name: ChannelId, rate: u32) -> Result<RecordingSink, WorkerError> {
        let (frames, input) = mpsc::channel(3);
        let (lifetime, lifetime_receiver) = oneshot::channel();
        let lease = Lease {
            state: SinkState::Active(frames),
            lifetime: Some(lifetime),
            port: self.clone(),
        };
        let (reply, response) = oneshot::channel();
        self.send(Command::Record {
            name,
            rate,
            input,
            lifetime: lifetime_receiver,
            reply,
        })?;
        let key = receive(response).await?;
        Ok(RecordingSink { key, lease })
    }
}

struct Entry {
    recorder: Recorder,
    input: mpsc::Receiver<Frame>,
    lifetime: oneshot::Receiver<()>,
    failure: Option<StorageError>,
}

pub(super) struct RecordingSet {
    root: PathBuf,
    generation: u64,
    entries: BTreeMap<RecordingKey, Entry>,
}

impl RecordingSet {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn begin(
        &mut self,
        name: ChannelId,
        rate: u32,
        input: mpsc::Receiver<Frame>,
        lifetime: oneshot::Receiver<()>,
    ) -> Result<RecordingKey, StorageError> {
        if self.entries.len() >= 64 {
            return Err(StorageError::Capacity);
        }
        if self.entries.keys().any(|key| key.name == name) {
            return Err(StorageError::InvalidName);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::Capacity)?;
        let recorder = Recorder::create(&self.root, name.as_str(), rate)?;
        self.generation = generation;
        let key = RecordingKey { name, generation };
        self.entries.insert(
            key.clone(),
            Entry {
                recorder,
                input,
                lifetime,
                failure: None,
            },
        );
        Ok(key)
    }

    pub fn pump(&mut self) {
        let mut cancelled = Vec::with_capacity(self.entries.len());
        for (key, entry) in &mut self.entries {
            match entry.lifetime.try_recv() {
                Ok(()) | Err(oneshot::error::TryRecvError::Closed) => {
                    cancelled.push(key.clone());
                    continue;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
            if entry.failure.is_some() {
                continue;
            }
            for _ in 0..3 {
                match entry.input.try_recv() {
                    Ok(frame) => {
                        if let Err(error) = entry.recorder.write(frame) {
                            tracing::warn!(recording = %key.name, generation = key.generation, %error, "recording write failed");
                            entry.failure = Some(error);
                            entry.input.close();
                            break;
                        }
                    }
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
        }
        for key in cancelled {
            if let Err(error) = self.cancel(&key) {
                tracing::warn!(recording = %key.name, generation = key.generation, %error, "recording cancellation failed");
            }
        }
    }

    pub fn finish(&mut self, key: &RecordingKey, store: &mut Store) -> Result<(), StorageError> {
        let entry = self.entries.remove(key).ok_or(StorageError::InvalidState)?;
        if let Some(error) = entry.failure {
            return Err(error);
        }
        entry.recorder.finish()?;
        store.recording_finished(key.name.as_str())
    }

    pub fn cancel(&mut self, key: &RecordingKey) -> Result<(), StorageError> {
        self.entries
            .remove(key)
            .ok_or(StorageError::InvalidState)?
            .recorder
            .cancel()
    }
}

#[cfg(test)]
mod tests {
    use super::{Lease, RecordingKey, RecordingSet, RecordingSink, SinkState};
    use crate::storage::{StoragePort, WorkerError};
    use rusterisk_api::ChannelId;
    use rusterisk_media::Frame;
    use rusterisk_storage::{StorageError, Store};
    use tokio::sync::{mpsc, oneshot, watch};

    fn name() -> Result<ChannelId, WorkerError> {
        ChannelId::try_from("bounded".to_owned()).map_err(|_| WorkerError::Invalid)
    }

    #[tokio::test]
    async fn an_overflow_is_terminal_even_when_audio_capacity_becomes_available(
    ) -> Result<(), WorkerError> {
        let (commands, _receiver) = mpsc::channel(64);
        let (_stop, stopped) = watch::channel(false);
        let (wake, _notifications) = mpsc::channel(1);
        let port = StoragePort {
            commands,
            stop: stopped,
            wake,
        };
        let (frames, mut input) = mpsc::channel(3);
        let (lifetime, mut owner) = oneshot::channel();
        let mut sink = RecordingSink {
            key: RecordingKey {
                name: name()?,
                generation: 1,
            },
            lease: Lease {
                state: SinkState::Active(frames),
                lifetime: Some(lifetime),
                port,
            },
        };
        let frame = Frame::new(&[1; 160], 8000).map_err(StorageError::from)?;
        for _ in 0..3 {
            sink.try_write(frame)?;
        }
        assert!(matches!(sink.try_write(frame), Err(WorkerError::Capacity)));
        assert!(input.try_recv().is_ok());
        assert!(matches!(sink.try_write(frame), Err(WorkerError::Capacity)));
        assert!(matches!(sink.finish().await, Err(WorkerError::Capacity)));
        assert!(matches!(
            owner.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        Ok(())
    }

    #[test]
    fn stale_operations_cannot_remove_a_reused_recording_name() -> Result<(), WorkerError> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(directory.path())?;
        let mut recordings = RecordingSet::new(directory.path().to_owned());
        let (_frames, input) = mpsc::channel(3);
        let (lifetime, receiver) = oneshot::channel();
        let stale = recordings.begin(name()?, 8000, input, receiver)?;
        drop(lifetime);
        recordings.pump();
        assert!(!directory.path().join("recordings/bounded.partial").exists());
        let (frames, input) = mpsc::channel(3);
        let (_lifetime, receiver) = oneshot::channel();
        let fresh = recordings.begin(name()?, 8000, input, receiver)?;
        assert!(matches!(
            recordings.cancel(&stale),
            Err(StorageError::InvalidState)
        ));
        assert!(matches!(
            recordings.finish(&stale, &mut store),
            Err(StorageError::InvalidState)
        ));
        frames
            .try_send(Frame::new(&[2; 160], 8000).map_err(StorageError::from)?)
            .map_err(|_| WorkerError::Capacity)?;
        recordings.pump();
        recordings.finish(&fresh, &mut store)?;
        assert_eq!(store.recordings()?.len(), 1);
        Ok(())
    }
}
