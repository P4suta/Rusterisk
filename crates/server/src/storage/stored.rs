use super::file::FileSet;
use super::{respond, MusicPreparationError, PreparedMusic, StoragePort, WorkerError};
use rusterisk_api::{ChannelId, Sound, StoredRecording};
use rusterisk_storage::{RecordingChunk, StorageError, Store};
use tokio::sync::{mpsc, oneshot};

pub(super) enum StoredCommand {
    PrepareMusic {
        classes: rusterisk_config::MusicOnHold,
        reply: oneshot::Sender<Result<PreparedMusic, MusicPreparationError>>,
    },
    Sounds {
        language: Option<String>,
        format: Option<String>,
        reply: oneshot::Sender<Result<Vec<Sound>, StorageError>>,
    },
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

pub(super) fn execute(
    store: &mut Store,
    files: &mut FileSet,
    root: &std::path::Path,
    command: StoredCommand,
    mut service: impl FnMut(&mut Store, &mut FileSet) -> Result<(), StorageError>,
) {
    match command {
        StoredCommand::PrepareMusic { classes, reply } => {
            if !reply.is_closed() {
                let result = super::music::prepare(root, classes, || {
                    if reply.is_closed() {
                        return Err(StorageError::Io(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "music requester disconnected",
                        )));
                    }
                    service(store, files)
                });
                if !reply.is_closed() {
                    if let Err(error) = &result {
                        tracing::warn!(%error, "music preparation failed");
                    }
                    if reply.send(result).is_err() {
                        tracing::debug!("music requester disconnected");
                    }
                }
            }
        }
        StoredCommand::Sounds {
            language,
            format,
            reply,
        } => {
            if !reply.is_closed() {
                let result =
                    super::sounds::catalogue(root, language.as_deref(), format.as_deref(), || {
                        if reply.is_closed() {
                            return Err(StorageError::Io(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "sound requester disconnected",
                            )));
                        }
                        service(store, files)
                    });
                if !reply.is_closed() {
                    respond(reply, result);
                }
            }
        }
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
