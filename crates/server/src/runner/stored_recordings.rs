use super::{Controller, Credentials, Request, Runner};
use crate::storage::{RecordingFile, StoragePort, WorkerError};
use crate::{ControlError, Outcome};
use rusterisk_api::ChannelId;
use rusterisk_storage::StorageError;
use tokio::sync::oneshot;

pub(crate) enum Operation {
    List,
    Get(ChannelId),
    File(ChannelId),
    Copy {
        source: ChannelId,
        destination: ChannelId,
    },
    Delete(ChannelId),
}

pub(crate) enum Response {
    Json(Outcome),
    File(RecordingFile),
}

impl Operation {
    fn read_only(&self) -> bool {
        match self {
            Self::List | Self::Get(_) | Self::File(_) => true,
            Self::Delete(_) | Self::Copy { .. } => false,
        }
    }
}

fn failure(error: WorkerError) -> ControlError {
    if matches!(&error, WorkerError::Storage(StorageError::NotFound)) {
        ControlError::NotFound("Recording not found")
    } else if matches!(&error, WorkerError::Storage(StorageError::Exists)) {
        ControlError::Conflict("A recording with the same name already exists on the system")
    } else if matches!(
        &error,
        WorkerError::Capacity | WorkerError::Timeout | WorkerError::Storage(StorageError::Capacity)
    ) {
        ControlError::Capacity
    } else if matches!(
        &error,
        WorkerError::Invalid | WorkerError::Storage(StorageError::InvalidName)
    ) {
        ControlError::Invalid("Invalid recording name")
    } else {
        ControlError::Adapter(error.to_string())
    }
}

async fn execute(storage: StoragePort, operation: Operation) -> Result<Response, ControlError> {
    match operation {
        Operation::Copy { source, destination } => Outcome::json(storage.copy_recording(source, destination).await.map_err(failure)?).map(Response::Json),
        Operation::List => Outcome::json(storage.recordings().await.map_err(failure)?)
            .map(Response::Json),
        Operation::Get(name) => Outcome::json(storage.recording(name).await.map_err(failure)?)
            .map(Response::Json),
        Operation::File(name) => storage.recording_file(name).await.map(Response::File).map_err(|error| {
            if matches!(&error, WorkerError::Storage(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied) {
                ControlError::Forbidden
            } else {
                failure(error)
            }
        }),
        Operation::Delete(name) => {
            storage.delete_recording(name).await.map_err(failure)?;
            Ok(Response::Json(Outcome::empty()))
        }
    }
}

async fn reply_query<F: std::future::Future<Output = Result<Response, ControlError>>>(
    mut reply: oneshot::Sender<Result<Response, ControlError>>,
    query: F,
) {
    tokio::select! {
        biased;
        () = reply.closed() => tracing::debug!("stored recording requester disconnected during its query"),
        result = query => {
            if reply.send(result).is_err() {
                tracing::debug!("stored recording requester disconnected");
            }
        }
    }
}

impl Controller {
    pub(crate) async fn stored_recording(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        operation: Operation,
    ) -> Result<Response, ControlError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::StoredRecording {
                credentials,
                origin,
                operation,
                reply,
            })
            .map_err(ControlError::queue)?;
        self.receive(response).await
    }
}

impl Runner {
    pub(super) fn stored_request(
        &mut self,
        credentials: Credentials,
        origin: Option<String>,
        operation: Operation,
        reply: oneshot::Sender<Result<Response, ControlError>>,
    ) {
        if reply.is_closed() {
            return;
        }
        let admitted = self
            .authorize(&credentials, origin.as_deref(), operation.read_only())
            .and_then(|()| {
                if self.stored_jobs.len() >= 16 {
                    return Err(ControlError::Capacity);
                }
                self.storage
                    .as_ref()
                    .map(|storage| storage.port.clone())
                    .ok_or(ControlError::Unsupported)
            });
        match admitted {
            Ok(storage) => {
                self.stored_jobs
                    .spawn(reply_query(reply, execute(storage, operation)));
            }
            Err(error) => {
                if reply.send(Err(error)).is_err() {
                    tracing::debug!("stored recording requester disconnected before admission");
                }
            }
        }
    }

    pub(super) async fn close_stored_queries(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), ControlError> {
        let mut outcome = Ok(());
        while !self.stored_jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.stored_jobs.join_next()).await {
                Ok(Some(Ok(()))) | Ok(None) => {}
                Ok(Some(Err(error))) => {
                    if outcome.is_ok() {
                        outcome = Err(ControlError::Adapter(error.to_string()));
                    }
                }
                Err(error) => {
                    self.stored_jobs.abort_all();
                    while let Some(result) = self.stored_jobs.join_next().await {
                        if let Err(error) = result {
                            tracing::debug!(%error, "stored recording task canceled during shutdown");
                        }
                    }
                    if outcome.is_ok() {
                        outcome = Err(ControlError::Adapter(error.to_string()));
                    }
                    break;
                }
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::{reply_query, Response};
    use crate::{ControlError, Controller};
    use rusterisk_config::Config;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn dropping_a_requester_releases_its_pending_storage_query() -> Result<(), ControlError> {
        let (reply, response) = oneshot::channel();
        let (lifetime, mut released) = oneshot::channel::<()>();
        let (entered, started) = oneshot::channel();
        let query = tokio::spawn(reply_query(reply, async move {
            let _lifetime = lifetime;
            entered.send(()).map_err(|_| ControlError::Capacity)?;
            std::future::pending::<Result<Response, ControlError>>().await
        }));
        started.await.map_err(|_| ControlError::Capacity)?;
        drop(response);
        rusterisk_test_support::Wait::Ready
            .bounded(query)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
        assert!(matches!(
            released.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_joins_remaining_queries_after_another_query_has_been_canceled(
    ) -> Result<(), ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=password\n",
            "",
            "",
            "",
        )
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
        let (_, mut runner, _) = Controller::new(config, 1);
        runner
            .stored_jobs
            .spawn(std::future::pending::<()>())
            .abort();
        let (lifetime, mut released) = oneshot::channel::<()>();
        runner.stored_jobs.spawn(async move {
            let _lifetime = lifetime;
            std::future::pending::<()>().await;
        });
        assert!(runner
            .close_stored_queries(tokio::time::Instant::now() + Duration::from_millis(50))
            .await
            .is_err());
        assert!(runner.stored_jobs.is_empty());
        assert!(matches!(
            released.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        Ok(())
    }
}
