use super::{respond, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, StoredRecording};
use rusterisk_storage::{StorageError, Store};
use tokio::sync::{mpsc, oneshot};

pub(super) enum StoredCommand {
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

pub(super) fn execute(store: &mut Store, command: StoredCommand) {
    match command {
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
