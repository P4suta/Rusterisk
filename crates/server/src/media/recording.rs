use crate::storage::{RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::MediaTaskToken;
use rusterisk_media::{Frame, MediaError};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

#[derive(Clone, PartialEq, Eq)]
pub enum RecordingTarget {
    Channel(CallKey),
    Bridge(u64),
}

#[derive(Clone, Copy)]
pub enum RecordingEnd {
    Finish,
    Cancel,
}

struct Capture {
    target: RecordingTarget,
    sink: RecordingSink,
    completed: oneshot::Sender<Result<(), WorkerError>>,
}

pub(super) struct Recordings {
    captures: BTreeMap<MediaTaskToken, Capture>,
    pub workers: JoinSet<()>,
}

impl Recordings {
    pub fn new() -> Self {
        Self {
            captures: BTreeMap::new(),
            workers: JoinSet::new(),
        }
    }

    pub fn begin(
        &mut self,
        token: MediaTaskToken,
        target: RecordingTarget,
        sink: RecordingSink,
        completed: oneshot::Sender<Result<(), WorkerError>>,
    ) -> Result<(), MediaError> {
        if completed.is_closed() {
            return Err(MediaError::UnknownStream);
        }
        if self.captures.len() + self.workers.len() >= 64 {
            return Err(MediaError::Capacity);
        }
        if self.captures.contains_key(&token) {
            return Err(MediaError::InvalidRoute);
        }
        self.captures.insert(
            token,
            Capture {
                target,
                sink,
                completed,
            },
        );
        Ok(())
    }

    pub fn end(&mut self, token: MediaTaskToken, mode: RecordingEnd) {
        if let Some(capture) = self.captures.remove(&token) {
            self.workers.spawn(async move {
                let result = match mode {
                    RecordingEnd::Finish => capture.sink.finish().await,
                    RecordingEnd::Cancel => capture.sink.cancel().await,
                };
                if let Err(error) = &result {
                    tracing::warn!(?token, %error, "recording ended with an error");
                }
                if capture.completed.send(result).is_err() {
                    tracing::debug!(?token, "recording result receiver disconnected");
                }
            });
        }
    }

    pub fn release(&mut self, key: &CallKey) {
        let tokens = self
            .captures
            .iter()
            .filter_map(|(token, capture)| {
                (capture.target == RecordingTarget::Channel(key.clone())).then_some(*token)
            })
            .collect::<Vec<_>>();
        for token in tokens {
            self.end(token, RecordingEnd::Finish);
        }
    }

    pub fn channel(&mut self, key: &CallKey, frame: Frame) {
        let mut failed = Vec::new();
        for (token, capture) in &mut self.captures {
            if capture.target == RecordingTarget::Channel(key.clone()) {
                if let Err(error) = capture.sink.try_write(frame) {
                    tracing::warn!(?token, %error, "recording audio refused");
                    failed.push(*token);
                }
            }
        }
        for token in failed {
            self.end(token, RecordingEnd::Finish);
        }
    }

    pub fn bridges(&mut self, frames: &[(u64, Frame)]) -> Result<(), MediaError> {
        let silence = Frame::new(&[0; 960], 48000)?;
        let mut finished = Vec::new();
        for (token, capture) in &mut self.captures {
            if capture.completed.is_closed() {
                finished.push((*token, RecordingEnd::Cancel));
                continue;
            }
            if let RecordingTarget::Bridge(id) = capture.target {
                let frame = frames
                    .iter()
                    .find_map(|(bridge, frame)| (*bridge == id).then_some(*frame))
                    .unwrap_or(silence);
                if let Err(error) = capture.sink.try_write(frame) {
                    tracing::warn!(?token, %error, "bridge recording audio refused");
                    finished.push((*token, RecordingEnd::Finish));
                }
            }
        }
        for (token, mode) in finished {
            self.end(token, mode);
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), MediaError> {
        let tokens = self.captures.keys().copied().collect::<Vec<_>>();
        for token in tokens {
            self.end(token, RecordingEnd::Finish);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.workers.is_empty() {
            match tokio::time::timeout_at(deadline, self.workers.join_next()).await {
                Ok(Some(Ok(()))) | Ok(None) => {}
                Ok(Some(Err(error))) => return Err(MediaError::Transport(error.to_string())),
                Err(error) => {
                    self.workers.abort_all();
                    while let Some(result) = self.workers.join_next().await {
                        match result {
                            Ok(()) => {}
                            Err(error) => {
                                tracing::debug!(%error, "recording task cancelled during shutdown")
                            }
                        }
                    }
                    return Err(MediaError::Transport(error.to_string()));
                }
            }
        }
        Ok(())
    }
}
