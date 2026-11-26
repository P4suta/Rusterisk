use super::{receive, respond, Command, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, RecordingIfExists};
use rusterisk_media::Frame;
use rusterisk_storage::{RecordingPreparation, StorageError, Store};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

const MAX_RECORDINGS: usize = 64;
const AUDIO_SLOTS: usize = 3;
pub(super) const MESSAGE_CAPACITY: usize = MAX_RECORDINGS * (AUDIO_SLOTS + 1);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RecordingKey(u64);

type Reply = oneshot::Sender<Result<(), StorageError>>;

#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/order-recording-audio-and-finalization.md: Keep accepted audio inline with terminal messages."
)]
pub(super) enum RecordingMessage {
    Audio {
        key: RecordingKey,
        frame: Frame,
        credit: mpsc::OwnedPermit<()>,
    },
    Finish {
        key: RecordingKey,
        trim_milliseconds: u64,
        reply: Reply,
        lifetime: oneshot::Sender<()>,
    },
    Cancel {
        key: RecordingKey,
        reply: Option<Reply>,
        lifetime: oneshot::Sender<()>,
    },
}

#[derive(Clone, Copy)]
enum SinkFailure {
    Capacity,
    Closed,
}

impl SinkFailure {
    fn from_send<T>(error: mpsc::error::TrySendError<T>) -> Self {
        match error {
            mpsc::error::TrySendError::Full(_) => Self::Capacity,
            mpsc::error::TrySendError::Closed(_) => Self::Closed,
        }
    }
}

impl From<SinkFailure> for WorkerError {
    fn from(failure: SinkFailure) -> Self {
        match failure {
            SinkFailure::Capacity => Self::Capacity,
            SinkFailure::Closed => Self::Closed,
        }
    }
}

struct Lease {
    key: Option<RecordingKey>,
    terminal: Option<(mpsc::OwnedPermit<RecordingMessage>, oneshot::Sender<()>)>,
    port: StoragePort,
}

impl Lease {
    fn submit(
        &mut self,
        make: impl FnOnce(RecordingKey, oneshot::Sender<()>) -> RecordingMessage,
    ) -> Result<(), WorkerError> {
        let key = self.key.ok_or(WorkerError::Invalid)?;
        let (slot, lifetime) = self.terminal.take().ok_or(WorkerError::Closed)?;
        slot.send(make(key, lifetime));
        self.port.wake();
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some((slot, lifetime)) = self.terminal.take() {
            if let Some(key) = self.key {
                slot.send(RecordingMessage::Cancel {
                    key,
                    reply: None,
                    lifetime,
                });
            } else {
                drop(lifetime);
            }
            self.port.wake();
        }
    }
}

pub struct RecordingSink {
    key: RecordingKey,
    credit: mpsc::Sender<()>,
    failure: Option<SinkFailure>,
    lease: Lease,
}

#[derive(Default)]
pub(crate) struct RecordingBatch {
    wake: Option<mpsc::Sender<()>>,
}

impl RecordingBatch {
    fn notify(&mut self, port: &StoragePort) {
        if self
            .wake
            .as_ref()
            .is_some_and(|wake| wake.same_channel(&port.wake))
        {
            return;
        }
        self.flush();
        self.wake = Some(port.wake.clone());
    }

    pub fn flush(&mut self) {
        if let Some(wake) = self.wake.take() {
            super::notify(&wake);
        }
    }
}

impl Drop for RecordingBatch {
    fn drop(&mut self) {
        self.flush();
    }
}

impl RecordingSink {
    pub fn try_write(&mut self, frame: Frame) -> Result<(), WorkerError> {
        self.write_in_batch(frame, &mut RecordingBatch::default())
    }

    pub(crate) fn write_in_batch(
        &mut self,
        frame: Frame,
        batch: &mut RecordingBatch,
    ) -> Result<(), WorkerError> {
        if *self.lease.port.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        if let Some(failure) = self.failure {
            return Err(failure.into());
        }
        let result = self
            .credit
            .clone()
            .try_reserve_owned()
            .map_err(SinkFailure::from_send)
            .and_then(|credit| {
                self.lease
                    .port
                    .recording
                    .try_send(RecordingMessage::Audio {
                        key: self.key,
                        frame,
                        credit,
                    })
                    .map_err(SinkFailure::from_send)
            });
        if let Err(failure) = result {
            self.failure = Some(failure);
            return Err(failure.into());
        }
        batch.notify(&self.lease.port);
        Ok(())
    }

    pub async fn finish(self) -> Result<(), WorkerError> {
        self.finish_with_tail_trim(0).await
    }

    pub async fn finish_with_tail_trim(
        mut self,
        trim_milliseconds: u64,
    ) -> Result<(), WorkerError> {
        if matches!(self.failure, Some(SinkFailure::Capacity)) {
            return Err(WorkerError::Capacity);
        }
        let (reply, response) = oneshot::channel();
        self.lease
            .submit(|key, lifetime| RecordingMessage::Finish {
                key,
                trim_milliseconds,
                reply,
                lifetime,
            })?;
        receive(response).await
    }

    pub async fn cancel(mut self) -> Result<(), WorkerError> {
        let (reply, response) = oneshot::channel();
        self.lease
            .submit(|key, lifetime| RecordingMessage::Cancel {
                key,
                reply: Some(reply),
                lifetime,
            })?;
        receive(response).await
    }
}

impl StoragePort {
    pub async fn record(&self, name: ChannelId, rate: u32) -> Result<RecordingSink, WorkerError> {
        self.record_with_policy(name, rate, RecordingIfExists::Fail)
            .await
    }

    pub async fn record_with_policy(
        &self,
        name: ChannelId,
        rate: u32,
        policy: RecordingIfExists,
    ) -> Result<RecordingSink, WorkerError> {
        let slot = self
            .recording
            .clone()
            .try_reserve_owned()
            .map_err(SinkFailure::from_send)?;
        let (credit, input) = mpsc::channel(AUDIO_SLOTS);
        let (lifetime, lifetime_receiver) = oneshot::channel();
        let mut lease = Lease {
            key: None,
            terminal: Some((slot, lifetime)),
            port: self.clone(),
        };
        let (reply, response) = oneshot::channel();
        self.send(Command::Record {
            name,
            rate,
            policy,
            input,
            lifetime: lifetime_receiver,
            reply,
        })?;
        let key = receive(response).await?;
        lease.key = Some(key);
        Ok(RecordingSink {
            key,
            credit,
            failure: None,
            lease,
        })
    }
}

struct Entry {
    name: ChannelId,
    recorder: RecordingPreparation,
    admission: Option<oneshot::Sender<Result<RecordingKey, StorageError>>>,
    input: mpsc::Receiver<()>,
    lifetime: oneshot::Receiver<()>,
    failure: Option<StorageError>,
}

pub(super) struct RecordingSet {
    root: PathBuf,
    generation: u64,
    entries: BTreeMap<RecordingKey, Entry>,
    messages: mpsc::Receiver<RecordingMessage>,
}

impl RecordingSet {
    pub fn new(root: PathBuf, messages: mpsc::Receiver<RecordingMessage>) -> Self {
        Self {
            root,
            generation: 0,
            entries: BTreeMap::new(),
            messages,
        }
    }

    pub fn begin(
        &mut self,
        name: ChannelId,
        rate: u32,
        policy: RecordingIfExists,
        input: mpsc::Receiver<()>,
        lifetime: oneshot::Receiver<()>,
    ) -> Result<RecordingKey, StorageError> {
        if self.entries.len() >= MAX_RECORDINGS {
            return Err(StorageError::Capacity);
        }
        if self.entries.values().any(|entry| entry.name == name) {
            return Err(StorageError::Exists);
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(StorageError::Capacity)?;
        let recorder = RecordingPreparation::create(&self.root, name.as_str(), rate, policy)?;
        self.generation = generation;
        let key = RecordingKey(generation);
        self.entries.insert(
            key,
            Entry {
                name,
                recorder,
                admission: None,
                input,
                lifetime,
                failure: None,
            },
        );
        Ok(key)
    }

    pub fn admit(
        &mut self,
        key: RecordingKey,
        reply: oneshot::Sender<Result<RecordingKey, StorageError>>,
    ) {
        let Some(entry) = self.entries.get_mut(&key) else {
            respond(reply, Err(StorageError::InvalidState));
            return;
        };
        if entry.admission.is_some() {
            respond(reply, Err(StorageError::InvalidState));
        } else if entry.recorder.is_ready() {
            if reply.send(Ok(key)).is_err() {
                if let Err(error) = self.cancel(&key) {
                    tracing::warn!(%error, "cancelled recording startup cleanup failed");
                }
            }
        } else {
            entry.admission = Some(reply);
        }
    }

    pub fn is_preparing(&self) -> bool {
        self.entries
            .values()
            .any(|entry| !entry.recorder.is_ready())
    }

    pub fn pump(&mut self, store: &mut Store) {
        let mut cancelled = Vec::new();
        for (key, entry) in &mut self.entries {
            if entry
                .admission
                .as_ref()
                .is_some_and(oneshot::Sender::is_closed)
                || !matches!(
                    entry.lifetime.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                )
            {
                cancelled.push(*key);
                continue;
            }
            if !entry.recorder.is_ready() {
                if let Err(error) = entry.recorder.advance() {
                    if let Some(reply) = entry.admission.take() {
                        respond(reply, Err(error));
                    } else {
                        tracing::warn!(%error, "recording copy failed before admission");
                    }
                    cancelled.push(*key);
                    continue;
                }
                if !entry.recorder.is_ready() {
                    continue;
                }
            }
            if let Some(reply) = entry.admission.take() {
                if reply.send(Ok(*key)).is_err() {
                    cancelled.push(*key);
                }
            }
        }
        for key in cancelled {
            if let Err(error) = self.cancel(&key) {
                tracing::warn!(generation = key.0, %error, "recording cancellation failed");
            }
        }
        for _ in 0..MESSAGE_CAPACITY {
            let Ok(message) = self.messages.try_recv() else {
                break;
            };
            match message {
                RecordingMessage::Audio { key, frame, credit } => {
                    if let Some(entry) = self.entries.get_mut(&key) {
                        if entry.failure.is_none() {
                            if let Err(error) = entry.recorder.write(frame) {
                                tracing::warn!(recording = %entry.name, generation = key.0, %error, "recording write failed");
                                entry.failure = Some(error);
                                entry.input.close();
                            }
                        }
                    }
                    drop(credit);
                }
                RecordingMessage::Finish {
                    key,
                    trim_milliseconds,
                    reply,
                    lifetime,
                } => {
                    respond(reply, self.finish(&key, store, trim_milliseconds));
                    drop(lifetime);
                }
                RecordingMessage::Cancel {
                    key,
                    reply,
                    lifetime,
                } => {
                    let result = self.cancel(&key);
                    match reply {
                        Some(reply) => respond(reply, result),
                        None => {
                            if let Err(error) = result {
                                tracing::warn!(generation = key.0, %error, "abandoned recording cleanup failed");
                            }
                        }
                    }
                    drop(lifetime);
                }
            }
        }
    }

    fn finish(
        &mut self,
        key: &RecordingKey,
        store: &mut Store,
        trim_milliseconds: u64,
    ) -> Result<(), StorageError> {
        let entry = self.entries.remove(key).ok_or(StorageError::InvalidState)?;
        if let Some(error) = entry.failure {
            return Err(error);
        }
        entry.recorder.finish_with_tail_trim(trim_milliseconds)?;
        store.recording_finished(entry.name.as_str())
    }

    fn cancel(&mut self, key: &RecordingKey) -> Result<(), StorageError> {
        self.entries
            .remove(key)
            .ok_or(StorageError::InvalidState)?
            .recorder
            .cancel()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Lease, RecordingKey, RecordingMessage, RecordingSet, RecordingSink, MESSAGE_CAPACITY,
    };
    use crate::storage::{StoragePort, WorkerError};
    use rusterisk_api::ChannelId;
    use rusterisk_media::Frame;
    use rusterisk_storage::{StorageError, Store};
    use tokio::sync::{mpsc, oneshot, watch};

    fn name() -> Result<ChannelId, WorkerError> {
        ChannelId::try_from("bounded".to_owned()).map_err(|_| WorkerError::Invalid)
    }

    #[tokio::test]
    async fn finish_preserves_the_last_accepted_sample_while_storage_is_stalled(
    ) -> Result<(), WorkerError> {
        let root = tempfile::tempdir()?;
        let mut store = Store::open(root.path())?;
        let (recording, inbox) = mpsc::channel(4);
        let mut recordings = RecordingSet::new(root.path().to_owned(), inbox);
        let mut playbacks = crate::storage::PlaybackSet::new(root.path().to_owned());
        let mut files = crate::storage::FileSet::new();
        let (commands, mut receiver) = mpsc::channel(1);
        let (stored, _stored_receiver) = mpsc::channel(1);
        let (_stop, stop) = watch::channel(false);
        let (wake, mut wakes) = mpsc::channel(1);
        let port = StoragePort {
            commands,
            stored,
            stop,
            wake,
            recording,
        };
        let (sink, admitted) = tokio::join!(port.record(name()?, 8000), async {
            let next = receiver.recv().await.ok_or(WorkerError::Closed)?;
            crate::storage::command(&mut store, &mut recordings, &mut playbacks, next);
            Ok::<_, WorkerError>(())
        });
        admitted?;
        let mut sink = sink?;
        let _busy = port
            .commands
            .try_reserve()
            .map_err(|_| WorkerError::Capacity)?;
        wakes.try_recv().map_err(|_| WorkerError::Invalid)?;
        let mut batch = super::RecordingBatch::default();
        for sample in [101, 202, 303] {
            let frame = Frame::new(&[sample; 160], 8000).map_err(StorageError::from)?;
            sink.write_in_batch(frame, &mut batch)?;
            assert!(wakes.is_empty());
        }
        batch.flush();
        assert_eq!(wakes.try_recv(), Ok(()));
        assert!(wakes.is_empty());
        let (finished, ()) = tokio::join!(biased; sink.finish(), async {
            crate::storage::drain_commands(&mut store, &mut recordings, &mut playbacks, &mut files, &mut receiver);
            recordings.pump(&mut store);
        });
        finished?;
        let mut reader =
            rusterisk_storage::WaveReader::open(&root.path().join("recordings/bounded.wav"))?;
        for sample in [101, 202, 303] {
            assert_eq!(
                reader.read_frame()?.ok_or(WorkerError::Invalid)?.samples(),
                [sample; 160]
            );
        }
        assert!(reader.read_frame()?.is_none());
        Ok(())
    }

    fn queue_audio(
        messages: &mpsc::Sender<RecordingMessage>,
        credits: mpsc::Sender<()>,
        key: RecordingKey,
        sample: i16,
    ) -> Result<(), WorkerError> {
        let credit = credits
            .try_reserve_owned()
            .map_err(|_| WorkerError::Capacity)?;
        let frame = Frame::new(&[sample; 160], 8000).map_err(StorageError::from)?;
        messages
            .try_send(RecordingMessage::Audio { key, frame, credit })
            .map_err(|_| WorkerError::Capacity)
    }

    #[test]
    fn append_copying_yields_between_bounded_steps_and_other_recordings_keep_progressing(
    ) -> Result<(), WorkerError> {
        let directory = tempfile::tempdir()?;
        let mut store = Store::open(directory.path())?;
        let mut original = rusterisk_storage::Recorder::create(directory.path(), "bounded", 8000)?;
        for _ in 0..100 {
            original.write(Frame::new(&[1234; 160], 8000).map_err(StorageError::from)?)?;
        }
        original.finish()?;
        let (messages, inbox) = mpsc::channel(MESSAGE_CAPACITY);
        let mut recordings = RecordingSet::new(directory.path().to_owned(), inbox);
        let (_frames, input) = mpsc::channel(3);
        let (_lifetime, receiver) = oneshot::channel();
        let append = recordings.begin(
            name()?,
            8000,
            rusterisk_api::RecordingIfExists::Append,
            input,
            receiver,
        )?;
        let path = directory.path().join("recordings/bounded.append");
        assert_eq!(std::fs::metadata(&path)?.len(), 44);
        let (reply, mut response) = oneshot::channel();
        recordings.admit(append, reply);
        recordings.pump(&mut store);
        assert_eq!(std::fs::metadata(&path)?.len(), 44 + 8192);
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let (frames, input) = mpsc::channel(3);
        let (_lifetime, receiver) = oneshot::channel();
        let other = recordings.begin(
            ChannelId::try_from("other".to_owned()).map_err(|_| WorkerError::Invalid)?,
            8000,
            rusterisk_api::RecordingIfExists::Fail,
            input,
            receiver,
        )?;
        queue_audio(&messages, frames, other, 5678)?;
        recordings.pump(&mut store);
        recordings.finish(&other, &mut store, 0)?;
        assert_eq!(
            rusterisk_storage::WaveReader::open(&directory.path().join("recordings/other.wav"))?
                .read_frame()?
                .ok_or(WorkerError::Invalid)?
                .samples(),
            [5678; 160]
        );
        assert_eq!(std::fs::metadata(&path)?.len(), 44 + 16384);
        recordings.pump(&mut store);
        recordings.pump(&mut store);
        assert!(response.try_recv().map_err(|_| WorkerError::Invalid)?? == append);
        recordings.finish(&append, &mut store, 0)?;
        assert_eq!(
            std::fs::metadata(directory.path().join("recordings/bounded.wav"))?.len(),
            32044
        );
        Ok(())
    }

    #[test]
    fn abandoned_append_admission_cancels_copying_before_the_next_storage_turn(
    ) -> Result<(), WorkerError> {
        let directory = tempfile::tempdir()?;
        let mut original = rusterisk_storage::Recorder::create(directory.path(), "bounded", 8000)?;
        for _ in 0..100 {
            original.write(Frame::new(&[1234; 160], 8000).map_err(StorageError::from)?)?;
        }
        original.finish()?;
        let previous = std::fs::read(directory.path().join("recordings/bounded.wav"))?;
        let (_messages, inbox) = mpsc::channel(MESSAGE_CAPACITY);
        let mut store = Store::open(directory.path())?;
        let mut recordings = RecordingSet::new(directory.path().to_owned(), inbox);
        let (_frames, input) = mpsc::channel(3);
        let (_lifetime, receiver) = oneshot::channel();
        let append = recordings.begin(
            name()?,
            8000,
            rusterisk_api::RecordingIfExists::Append,
            input,
            receiver,
        )?;
        let (reply, response) = oneshot::channel();
        recordings.admit(append, reply);
        drop(response);
        recordings.pump(&mut store);
        assert!(!directory.path().join("recordings/bounded.append").exists());
        assert!(!recordings.is_preparing());
        assert_eq!(
            std::fs::read(directory.path().join("recordings/bounded.wav"))?,
            previous
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_overflow_is_terminal_even_when_audio_capacity_becomes_available(
    ) -> Result<(), WorkerError> {
        let (commands, _receiver) = mpsc::channel(64);
        let (stored, _stored_receiver) = mpsc::channel(16);
        let (_stop, stopped) = watch::channel(false);
        let (wake, _notifications) = mpsc::channel(1);
        let (recording, mut messages) = mpsc::channel(MESSAGE_CAPACITY);
        let slot = recording
            .clone()
            .try_reserve_owned()
            .map_err(|_| WorkerError::Capacity)?;
        let port = StoragePort {
            commands,
            stored,
            stop: stopped,
            wake,
            recording,
        };
        let (credit, _input) = mpsc::channel(3);
        let (lifetime, mut owner) = oneshot::channel();
        let key = RecordingKey(1);
        let mut sink = RecordingSink {
            key,
            credit,
            failure: None,
            lease: Lease {
                key: Some(key),
                terminal: Some((slot, lifetime)),
                port,
            },
        };
        let frame = Frame::new(&[1; 160], 8000).map_err(StorageError::from)?;
        for _ in 0..3 {
            sink.try_write(frame)?;
        }
        assert!(matches!(sink.try_write(frame), Err(WorkerError::Capacity)));
        assert!(matches!(
            messages.try_recv(),
            Ok(RecordingMessage::Audio { .. })
        ));
        assert!(matches!(sink.try_write(frame), Err(WorkerError::Capacity)));
        assert!(matches!(sink.finish().await, Err(WorkerError::Capacity)));
        for _ in 0..2 {
            assert!(matches!(
                messages.try_recv(),
                Ok(RecordingMessage::Audio { .. })
            ));
        }
        assert!(matches!(
            messages.try_recv(),
            Ok(RecordingMessage::Cancel { .. })
        ));
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
        let (messages, inbox) = mpsc::channel(MESSAGE_CAPACITY);
        let mut recordings = RecordingSet::new(directory.path().to_owned(), inbox);
        let (_frames, input) = mpsc::channel(3);
        let (lifetime, receiver) = oneshot::channel();
        let stale = recordings.begin(
            name()?,
            8000,
            rusterisk_api::RecordingIfExists::Fail,
            input,
            receiver,
        )?;
        drop(lifetime);
        recordings.pump(&mut store);
        assert!(!directory.path().join("recordings/bounded.partial").exists());
        let (frames, input) = mpsc::channel(3);
        let (_lifetime, receiver) = oneshot::channel();
        let fresh = recordings.begin(
            name()?,
            8000,
            rusterisk_api::RecordingIfExists::Fail,
            input,
            receiver,
        )?;
        assert!(matches!(
            recordings.cancel(&stale),
            Err(StorageError::InvalidState)
        ));
        assert!(matches!(
            recordings.finish(&stale, &mut store, 0),
            Err(StorageError::InvalidState)
        ));
        queue_audio(&messages, frames, fresh, 2)?;
        recordings.pump(&mut store);
        recordings.finish(&fresh, &mut store, 0)?;
        assert_eq!(store.recordings()?.len(), 1);
        Ok(())
    }
}
