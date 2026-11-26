use super::{Controller, Credentials, Request, Runner};
use crate::{
    storage::{StoragePort, WorkerError},
    ControlError, Outcome,
};
use rusterisk_api::SoundId;
use rusterisk_storage::StorageError;
use tokio::sync::oneshot;

pub(crate) enum Query {
    List {
        language: Option<String>,
        format: Option<String>,
    },
    Get(SoundId),
}

fn failure(error: WorkerError) -> ControlError {
    match error {
        WorkerError::Capacity
        | WorkerError::Timeout
        | WorkerError::Storage(StorageError::Capacity) => ControlError::Capacity,
        WorkerError::Invalid => ControlError::Invalid("Invalid sound query"),
        error @ (WorkerError::Storage(_) | WorkerError::Closed | WorkerError::Thread) => {
            ControlError::Adapter(error.to_string())
        }
    }
}

async fn execute(storage: StoragePort, query: Query) -> Result<Outcome, ControlError> {
    match query {
        Query::List { language, format } => {
            let sounds = storage
                .sounds(language.as_deref(), format.as_deref())
                .await
                .map_err(failure)?;
            if sounds.is_empty() {
                return Err(ControlError::NotFound(
                    "No sounds found that matched the query",
                ));
            }
            Outcome::json(sounds)
        }
        Query::Get(name) => {
            let sound = storage
                .sounds(None, None)
                .await
                .map_err(failure)?
                .into_iter()
                .find(|sound| sound.id == name.as_str())
                .ok_or(ControlError::NotFound("Sound not found"))?;
            Outcome::json(sound)
        }
    }
}

impl Controller {
    pub(crate) async fn sounds(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        query: Query,
    ) -> Result<Outcome, ControlError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::Sounds {
                credentials,
                origin,
                query,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(response).await
    }
}

impl Runner {
    pub(super) fn sound_request(
        &mut self,
        credentials: Credentials,
        origin: Option<String>,
        query: Query,
        mut reply: oneshot::Sender<Result<Outcome, ControlError>>,
    ) {
        if reply.is_closed() {
            return;
        }
        let storage = self
            .authorize(&credentials, origin.as_deref(), true)
            .and_then(|()| {
                if self.stored_jobs.len() >= 16 {
                    return Err(ControlError::Capacity);
                }
                self.storage
                    .as_ref()
                    .map(|storage| storage.port.clone())
                    .ok_or(ControlError::Unsupported)
            });
        match storage {
            Ok(storage) => {
                self.stored_jobs.spawn(async move {
                tokio::select! {
                    biased;
                    () = reply.closed() => tracing::debug!("sound requester disconnected during query"),
                    result = execute(storage, query) => {
                        if reply.send(result).is_err() { tracing::debug!("sound requester disconnected"); }
                    }
                }
            });
            }
            Err(error) => {
                if reply.send(Err(error)).is_err() {
                    tracing::debug!("sound requester disconnected before admission");
                }
            }
        }
    }
}
