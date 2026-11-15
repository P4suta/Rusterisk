use super::file::FileSet;
use super::{respond, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, StoredRecording};
use rusterisk_storage::{RecordingChunk, StorageError, Store};
use tokio::sync::{mpsc, oneshot};

pub(super) enum StoredCommand {
    Copy {
        source: ChannelId,
        destination: ChannelId,
        reply: oneshot::Sender<Result<StoredRecording, StorageError>>,
    },
    OpenFile {
        name: ChannelId,
        output: mpsc::Sender<RecordingChunk>,
        lifetime: oneshot::Receiver<()>,
        completed: oneshot::Sender<Result<(), StorageError>>,
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    List {
        reply: oneshot::Sender<Result<Vec<StoredRecording>, StorageError>>,
    },
    Get {
        name: ChannelId,
        reply: oneshot::Sender<Result<StoredRecording, StorageError>>,
    },
    Delete {
        name: ChannelId,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
}

impl StoragePort {
    pub(super) fn send_stored(&self, command: StoredCommand) -> Result<(), WorkerError> {
        if *self.stop.borrow() {
            return Err(WorkerError::Closed);
        }
        self.stored.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => WorkerError::Capacity,
            mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
        })?;
        self.wake();
        Ok(())
    }
}

pub(super) fn execute(store: &mut Store, files: &mut FileSet, command: StoredCommand) {
    match command {
        StoredCommand::Copy {
            source,
            destination,
            reply,
        } => files.copy(store, source, destination, reply),
        StoredCommand::OpenFile {
            name,
            output,
            lifetime,
            completed,
            reply,
        } => {
            if !reply.is_closed() {
                respond(reply, files.begin(store, name, output, lifetime, completed));
            }
        }
        StoredCommand::List { reply } => {
            if !reply.is_closed() {
                respond(reply, store.recordings());
            }
        }
        StoredCommand::Get { name, reply } => {
            if !reply.is_closed() {
                respond(reply, store.recording(name.as_str()));
            }
        }
        StoredCommand::Delete { name, reply } => {
            if !reply.is_closed() {
                respond(reply, store.delete_recording(name.as_str()));
            }
        }
    }
}
