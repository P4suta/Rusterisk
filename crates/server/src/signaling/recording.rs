use super::{failure, Signaling};
use crate::media::{RecordingEnd, RecordingTarget, Reservation};
use crate::storage::{PlaybackMedia, WorkerError};
use crate::{ControlError, Effect, MediaOwner, Signal};
use rusterisk_api::{ChannelId, RecordingOptions};
use rusterisk_core::MediaTaskToken;
use rusterisk_media::AudioTarget;
use rusterisk_storage::StorageError;
use std::time::Duration;

pub(super) struct Completion {
    owner: MediaOwner,
    token: MediaTaskToken,
    result: JobResult,
}

enum JobResult {
    Rejected(ControlError),
    Finished(Result<(), ControlError>),
}

fn storage_error(error: WorkerError) -> ControlError {
    if matches!(&error, WorkerError::Storage(StorageError::Exists)) {
        ControlError::Conflict("Recording already exists")
    } else if matches!(&error, WorkerError::Storage(StorageError::Unsupported)) {
        ControlError::Unsupported
    } else if matches!(
        &error,
        WorkerError::Capacity | WorkerError::Storage(StorageError::Capacity)
    ) {
        ControlError::Capacity
    } else {
        failure(error)
    }
}

impl Signaling {
    pub(super) async fn start_recording(
        &mut self,
        owner: MediaOwner,
        token: MediaTaskToken,
        name: ChannelId,
        options: RecordingOptions,
        now: u64,
    ) -> Result<(), ControlError> {
        let prepared = async {
            let storage = self.storage.clone().ok_or(ControlError::Unsupported)?;
            if self.recordings.len() >= 64 {
                return Err(ControlError::Capacity);
            }
            if let MediaOwner::Channel(key) = &owner {
                self.answer(key, now).await?;
            }
            Ok(storage)
        }
        .await;
        // An answer leaves its call waiting for the media task, and the recording starts after that wait.
        if let (Ok(_), MediaOwner::Channel(key)) = (&prepared, &owner) {
            if self.waits(key) {
                let key = key.clone();
                let effect = Effect::RecordMedia {
                    owner,
                    token,
                    name,
                    options,
                };
                if let Some(effect) = self.hold_first(&key, effect) {
                    return Err(ControlError::Adapter(format!(
                        "A waiting call refused its held effect: {effect:?}"
                    )));
                }
                return Ok(());
            }
        }
        let admitted = async {
            let storage = prepared?;
            let target = match &owner {
                MediaOwner::Channel(key) => RecordingTarget::Channel(key.clone()),
                MediaOwner::Bridge { generation, .. } => RecordingTarget::Bridge(*generation),
            };
            let Reservation {
                admission,
                completed,
            } = self.media.queue_recording(target, token).map_err(failure)?;
            let prompt = if options.beep {
                let target = match &owner {
                    MediaOwner::Channel(key) => AudioTarget::Channel(key.clone()),
                    MediaOwner::Bridge { generation, .. } => AudioTarget::Bridge(*generation),
                };
                Some(
                    self.media
                        .queue_playback(target, token, Default::default())
                        .map_err(failure)?,
                )
            } else {
                None
            };
            Ok((storage, admission, completed, prompt))
        }
        .await;
        let (storage, reserved, completed, prompt) = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                self.notify(Signal::RecordingAdmitted {
                    owner,
                    token,
                    result: Err(error),
                })
                .await?;
                return Ok(());
            }
        };
        let media = self.media.clone();
        let controller = self.controller.clone();
        self.recordings.spawn(async move {
            let admission = async {
                reserved.wait().await.map_err(failure)?;
                let prompt = match prompt {
                    Some(Reservation {
                        admission,
                        completed,
                    }) => {
                        admission.wait().await.map_err(failure)?;
                        Some(completed)
                    }
                    None => None,
                };
                let sink = storage.record_with_policy(name, 8000, options.if_exists).await.map_err(storage_error)?;
                let source = if options.beep {
                    match storage.play(PlaybackMedia::Sound(ChannelId::try_from("beep".to_owned())?.into())).await {
                        Ok(source) => Some(source),
                        Err(error) => {
                            tracing::warn!(?owner, ?token, %error, "recording prompt unavailable");
                            drop(prompt);
                            sink.cancel().await.map_err(storage_error)?;
                            media.stop_recording(token, RecordingEnd::Cancel).map_err(failure)?;
                            if !controller.is_stopping() { controller.signal(Signal::RecordingAdmitted { owner: owner.clone(), token, result: Ok(()) }).await?; }
                            return Ok(());
                        }
                    }
                } else { None };
                media.attach_recording_with_options(token, sink, options, prompt).await.map_err(failure)?;
                if let Some(source) = source { media.attach_playback(token, source).await.map_err(failure)?; }
                if !controller.is_stopping() { controller.signal(Signal::RecordingAdmitted { owner: owner.clone(), token, result: Ok(()) }).await?; }
                Ok(())
            }.await;
            let result = match admission {
                Ok(()) => JobResult::Finished(completed.await.map_err(failure).and_then(|result| result.map_err(storage_error))),
                Err(error) => {
                    if let Err(cleanup) = media.stop_recording(token, RecordingEnd::Cancel) { tracing::warn!(?owner, ?token, %cleanup, "recording admission cleanup deferred"); }
                    JobResult::Rejected(error)
                }
            };
            Completion { owner, token, result }
        });
        Ok(())
    }

    pub(super) async fn recording_completed(
        &mut self,
        completed: Completion,
    ) -> Result<(), ControlError> {
        let Completion {
            owner,
            token,
            result,
        } = completed;
        match result {
            JobResult::Rejected(error) => {
                tracing::warn!(?owner, ?token, %error, "recording admission failed");
                self.notify(Signal::RecordingAdmitted {
                    owner,
                    token,
                    result: Err(error),
                })
                .await
            }
            JobResult::Finished(result) => {
                if let Err(error) = &result {
                    tracing::warn!(?owner, ?token, %error, "recording storage failed");
                }
                self.notify(Signal::RecordingFinished {
                    owner,
                    token,
                    result,
                })
                .await
            }
        }
    }

    pub(super) async fn finish_recordings(&mut self) -> Result<(), ControlError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut failure_result = Ok(());
        while !self.recordings.is_empty() {
            match tokio::time::timeout_at(deadline, self.recordings.join_next()).await {
                Ok(Some(Ok(Completion {
                    owner,
                    token,
                    result,
                }))) => match result {
                    JobResult::Finished(Ok(())) => {}
                    JobResult::Rejected(error) => {
                        tracing::debug!(?owner, ?token, %error, "recording admission ended during shutdown")
                    }
                    JobResult::Finished(Err(error)) => {
                        tracing::error!(?owner, ?token, %error, "recording failed before shutdown");
                        if failure_result.is_ok() {
                            failure_result = Err(error);
                        }
                    }
                },
                Ok(None) => {}
                Ok(Some(Err(error))) => {
                    if failure_result.is_ok() {
                        failure_result = Err(failure(error));
                    }
                }
                Err(error) => {
                    self.recordings.abort_all();
                    while let Some(result) = self.recordings.join_next().await {
                        match result {
                            Ok(_) => {}
                            Err(error) => {
                                tracing::debug!(%error, "recording task cancelled during shutdown")
                            }
                        }
                    }
                    return Err(failure(error));
                }
            }
        }
        failure_result
    }
}
