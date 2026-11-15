use rusterisk_api::{ChannelId, StoredRecording};
pub use rusterisk_storage::CallOutcome;
use rusterisk_storage::{CallHistoryChange, CallRecord, StorageError, Store};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
mod history;
mod playback;
pub use history::HistoryReceipt;
mod recording;
use playback::PlaybackSet;
pub use playback::{PlaybackMedia, PlaybackSource};
pub use recording::RecordingSink;
use recording::{RecordingKey, RecordingSet};
use rusterisk_media::Frame;

#[derive(Debug)]
pub enum WorkerError {
    Storage(StorageError),
    Invalid,
    Capacity,
    Closed,
    Timeout,
    Thread,
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::Invalid => f.write_str("Invalid storage command"),
            Self::Capacity => f.write_str("Storage command capacity exceeded"),
            Self::Closed => f.write_str("Storage worker closed"),
            Self::Timeout => f.write_str("Storage operation timed out"),
            Self::Thread => f.write_str("Storage thread failed"),
        }
    }
}
impl std::error::Error for WorkerError {}
impl From<StorageError> for WorkerError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl From<std::io::Error> for WorkerError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(StorageError::Io(error))
    }
}

enum Command {
    History {
        changes: Vec<CallHistoryChange>,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    Records {
        limit: u16,
        reply: oneshot::Sender<Result<Vec<CallRecord>, StorageError>>,
    },
    Play {
        media: PlaybackMedia,
        output: mpsc::Sender<Frame>,
        lifetime: oneshot::Receiver<()>,
        completed: oneshot::Sender<Result<(), StorageError>>,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    Start {
        id: ChannelId,
        timestamp: String,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    Finish {
        id: ChannelId,
        outcome: CallOutcome,
        timestamp: String,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    Outcome {
        id: ChannelId,
        reply: oneshot::Sender<Result<CallOutcome, StorageError>>,
    },
    Recordings {
        reply: oneshot::Sender<Result<Vec<StoredRecording>, StorageError>>,
    },
    Record {
        name: ChannelId,
        rate: u32,
        policy: rusterisk_api::RecordingIfExists,
        input: mpsc::Receiver<Frame>,
        lifetime: oneshot::Receiver<()>,
        reply: oneshot::Sender<Result<RecordingKey, StorageError>>,
    },
    FinishRecording {
        key: RecordingKey,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    CancelRecording {
        key: RecordingKey,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
}

#[derive(Clone)]
pub struct StoragePort {
    commands: mpsc::Sender<Command>,
    stop: watch::Receiver<bool>,
    wake: mpsc::Sender<()>,
}

impl StoragePort {
    fn wake(&self) {
        match self.wake.try_send(()) {
            Ok(())
            | Err(mpsc::error::TrySendError::Full(_) | mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    fn send(&self, command: Command) -> Result<(), WorkerError> {
        if *self.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Capacity,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
            })?;
        self.wake();
        Ok(())
    }

    pub async fn start_call(&self, id: ChannelId, timestamp: &str) -> Result<(), WorkerError> {
        if timestamp.len() > 64 {
            return Err(WorkerError::Invalid);
        }
        let (reply, response) = oneshot::channel();
        self.send(Command::Start {
            id,
            timestamp: timestamp.into(),
            reply,
        })?;
        receive(response).await
    }

    pub async fn finish_call(
        &self,
        id: ChannelId,
        outcome: CallOutcome,
        timestamp: &str,
    ) -> Result<(), WorkerError> {
        if timestamp.len() > 64 || outcome == CallOutcome::Active {
            return Err(WorkerError::Invalid);
        }
        let (reply, response) = oneshot::channel();
        self.send(Command::Finish {
            id,
            outcome,
            timestamp: timestamp.into(),
            reply,
        })?;
        receive(response).await
    }

    pub async fn call_outcome(&self, id: ChannelId) -> Result<CallOutcome, WorkerError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Outcome { id, reply })?;
        receive(response).await
    }

    pub async fn recordings(&self) -> Result<Vec<StoredRecording>, WorkerError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Recordings { reply })?;
        receive(response).await
    }
}

async fn receive<T>(
    response: oneshot::Receiver<Result<T, StorageError>>,
) -> Result<T, WorkerError> {
    tokio::time::timeout(Duration::from_secs(5), response)
        .await
        .map_err(|_| WorkerError::Timeout)?
        .map_err(|_| WorkerError::Closed)?
        .map_err(WorkerError::Storage)
}

fn respond<T>(reply: oneshot::Sender<Result<T, StorageError>>, result: Result<T, StorageError>) {
    if let Err(error) = &result {
        tracing::warn!(%error, "storage operation failed");
    }
    if reply.send(result).is_err() {
        tracing::debug!("storage requester disconnected");
    }
}

fn command(
    store: &mut Store,
    recordings: &mut RecordingSet,
    playbacks: &mut PlaybackSet,
    command: Command,
) {
    match command {
        Command::History { changes, reply } => {
            if !reply.is_closed() {
                respond(reply, store.apply_history(&changes));
            }
        }
        Command::Records { limit, reply } => {
            if !reply.is_closed() {
                respond(reply, store.call_records(limit));
            }
        }
        Command::Play {
            media,
            output,
            lifetime,
            completed,
            reply,
        } => {
            if !reply.is_closed() {
                respond(reply, playbacks.begin(media, output, lifetime, completed));
            }
        }
        Command::Start {
            id,
            timestamp,
            reply,
        } => {
            if !reply.is_closed() {
                respond(reply, store.start_call(id.as_str(), &timestamp));
            }
        }
        Command::Finish {
            id,
            outcome,
            timestamp,
            reply,
        } => {
            if !reply.is_closed() {
                respond(reply, store.finish_call(id.as_str(), outcome, &timestamp));
            }
        }
        Command::Outcome { id, reply } => {
            if !reply.is_closed() {
                respond(reply, store.call_outcome(id.as_str()));
            }
        }
        Command::Recordings { reply } => {
            if !reply.is_closed() {
                respond(reply, store.recordings());
            }
        }
        Command::Record {
            name,
            rate,
            policy,
            input,
            lifetime,
            reply,
        } => {
            if !reply.is_closed() {
                match recordings.begin(name, rate, policy, input, lifetime) {
                    Ok(key) => recordings.admit(key, reply),
                    Err(error) => respond(reply, Err(error)),
                }
            }
        }
        Command::FinishRecording { key, reply } => {
            if !reply.is_closed() {
                respond(reply, recordings.finish(&key, store));
            }
        }
        Command::CancelRecording { key, reply } => {
            if !reply.is_closed() {
                respond(reply, recordings.cancel(&key));
            }
        }
    }
}

fn serve(
    mut store: Store,
    root: PathBuf,
    mut commands: mpsc::Receiver<Command>,
    mut wakes: mpsc::Receiver<()>,
    stop: watch::Receiver<bool>,
) {
    let mut recordings = RecordingSet::new(root.clone());
    let mut playbacks = PlaybackSet::new(root);
    loop {
        if *stop.borrow() {
            commands.close();
            while let Ok(next) = commands.try_recv() {
                recordings.pump();
                playbacks.pump();
                command(&mut store, &mut recordings, &mut playbacks, next);
            }
            recordings.pump();
            playbacks.pump();
            break;
        }
        if !recordings.is_preparing() && wakes.blocking_recv().is_none() {
            break;
        }
        for _ in 0..64 {
            match commands.try_recv() {
                Ok(next) => {
                    recordings.pump();
                    playbacks.pump();
                    command(&mut store, &mut recordings, &mut playbacks, next);
                    recordings.pump();
                    playbacks.pump();
                }
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                    break
                }
            }
        }
        recordings.pump();
        playbacks.pump();
    }
}

pub struct StorageWorker {
    thread: Option<JoinHandle<()>>,
    completed: Option<oneshot::Receiver<()>>,
    stop: watch::Sender<bool>,
    wake: mpsc::Sender<()>,
}

impl StorageWorker {
    pub async fn start(root: PathBuf) -> Result<(StoragePort, Self), WorkerError> {
        if root.as_os_str().as_encoded_bytes().len() > 4096 {
            return Err(WorkerError::Invalid);
        }
        let (commands, receiver) = mpsc::channel(64);
        let (wake, wakes) = mpsc::channel(1);
        let (stop, stopped) = watch::channel(false);
        let (ready, startup) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        let port = StoragePort {
            commands: commands.clone(),
            stop: stopped.clone(),
            wake: wake.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("rusterisk-storage".into())
            .spawn(move || {
                match Store::open(&root) {
                    Ok(store) => {
                        if ready.send(Ok(())).is_ok() {
                            serve(store, root, receiver, wakes, stopped);
                        }
                    }
                    Err(error) => {
                        if ready.send(Err(error)).is_err() {
                            tracing::debug!("storage startup requester disconnected");
                        }
                    }
                }
                if completed.send(()).is_err() {
                    tracing::debug!("storage supervisor disconnected");
                }
            })?;
        let worker = Self {
            thread: Some(thread),
            completed: Some(completion),
            stop,
            wake,
        };
        match receive(startup).await {
            Ok(()) => Ok((port, worker)),
            Err(primary) => {
                if let Err(cleanup) = worker.close().await {
                    tracing::warn!(%cleanup, "storage startup cleanup failed");
                }
                Err(primary)
            }
        }
    }

    fn begin_close(&self) {
        self.stop.send_replace(true);
        match self.wake.try_send(()) {
            Ok(())
            | Err(mpsc::error::TrySendError::Full(_) | mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    pub async fn close(mut self) -> Result<(), WorkerError> {
        self.begin_close();
        let completed = self.completed.take().ok_or(WorkerError::Thread)?;
        tokio::time::timeout(Duration::from_secs(5), completed)
            .await
            .map_err(|_| WorkerError::Timeout)?
            .map_err(|_| WorkerError::Thread)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(1)))
                .await
                .map_err(|_| WorkerError::Timeout)?;
        }
        self.thread
            .take()
            .ok_or(WorkerError::Thread)?
            .join()
            .map_err(|_| WorkerError::Thread)?;
        Ok(())
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        self.begin_close();
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, StoragePort, StorageWorker, WorkerError};
    use rusterisk_api::ChannelId;
    use rusterisk_storage::Store;
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    #[test]
    fn audio_wake_notifications_do_not_consume_storage_command_capacity() -> Result<(), WorkerError>
    {
        let (commands, receiver) = mpsc::channel(64);
        let (_stop, stopped) = watch::channel(false);
        let (wake, notifications) = mpsc::channel(1);
        let port = StoragePort {
            commands,
            stop: stopped,
            wake,
        };
        for _ in 0..1000 {
            port.wake();
        }
        assert_eq!(receiver.len(), 0);
        assert_eq!(notifications.len(), 1);
        let (reply, _response) = tokio::sync::oneshot::channel();
        port.send(Command::Recordings { reply })?;
        assert_eq!(receiver.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn queued_storage_work_is_bounded_before_the_worker_accepts_more(
    ) -> Result<(), WorkerError> {
        let (commands, receiver) = mpsc::channel(64);
        let (stop, stopped) = watch::channel(false);
        let (wake, _notifications) = mpsc::channel(1);
        let port = StoragePort {
            commands: commands.clone(),
            stop: stopped,
            wake,
        };
        for _ in 0..64 {
            let (reply, _response) = tokio::sync::oneshot::channel();
            commands
                .try_send(Command::Recordings { reply })
                .map_err(|_| WorkerError::Capacity)?;
        }
        let id = ChannelId::try_from("refused".to_owned()).map_err(|_| WorkerError::Invalid)?;
        assert!(matches!(
            port.start_call(id, "start").await,
            Err(WorkerError::Capacity)
        ));
        assert_eq!(receiver.len(), 64);
        stop.send_replace(true);
        assert!(matches!(port.recordings().await, Err(WorkerError::Closed)));
        Ok(())
    }

    #[tokio::test]
    async fn dropping_the_worker_releases_the_store_while_clients_remain_alive(
    ) -> Result<(), WorkerError> {
        let directory = tempfile::tempdir()?;
        let (port, mut worker) = StorageWorker::start(directory.path().to_owned()).await?;
        let completed = worker.completed.take().ok_or(WorkerError::Thread)?;
        drop(worker);
        assert!(matches!(port.recordings().await, Err(WorkerError::Closed)));
        tokio::time::timeout(Duration::from_secs(2), completed)
            .await
            .map_err(|_| WorkerError::Timeout)?
            .map_err(|_| WorkerError::Thread)?;
        let reopened = Store::open(directory.path())?;
        assert!(reopened.recordings()?.is_empty());
        Ok(())
    }
}
