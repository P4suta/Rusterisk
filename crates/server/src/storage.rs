use rusterisk_api::{ChannelId, StoredRecording};
pub use rusterisk_storage::CallOutcome;
use rusterisk_storage::{StorageError, Store};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

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
    Wake,
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
}

#[derive(Clone)]
pub struct StoragePort {
    commands: mpsc::Sender<Command>,
    stop: watch::Receiver<bool>,
}

impl StoragePort {
    fn send(&self, command: Command) -> Result<(), WorkerError> {
        if *self.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Capacity,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
            })
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

fn command(store: &mut Store, command: Command) {
    match command {
        Command::Wake => {}
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
    }
}

fn serve(mut store: Store, mut commands: mpsc::Receiver<Command>, stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            commands.close();
            while let Ok(next) = commands.try_recv() {
                command(&mut store, next);
            }
            break;
        }
        match commands.blocking_recv() {
            Some(next) => command(&mut store, next),
            None => break,
        }
    }
}

pub struct StorageWorker {
    thread: Option<JoinHandle<()>>,
    completed: Option<oneshot::Receiver<()>>,
    stop: watch::Sender<bool>,
    commands: mpsc::Sender<Command>,
}

impl StorageWorker {
    pub async fn start(root: PathBuf) -> Result<(StoragePort, Self), WorkerError> {
        if root.as_os_str().as_encoded_bytes().len() > 4096 {
            return Err(WorkerError::Invalid);
        }
        let (commands, receiver) = mpsc::channel(64);
        let (stop, stopped) = watch::channel(false);
        let (ready, startup) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        let port = StoragePort {
            commands: commands.clone(),
            stop: stopped.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("rusterisk-storage".into())
            .spawn(move || {
                match Store::open(&root) {
                    Ok(store) => {
                        if ready.send(Ok(())).is_ok() {
                            serve(store, receiver, stopped);
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
            commands,
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
        match self.commands.try_send(Command::Wake) {
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
