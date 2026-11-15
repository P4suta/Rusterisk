use crate::storage::{RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::MediaTaskToken;
use rusterisk_media::{Frame, MediaError};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
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
    source: Source,
    requested: Instant,
    completed: oneshot::Sender<Result<(), WorkerError>>,
}

enum Source {
    Waiting,
    Attached(RecordingSink),
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

    pub fn reserve(
        &mut self,
        token: MediaTaskToken,
        target: RecordingTarget,
        completed: oneshot::Sender<Result<(), WorkerError>>,
    ) -> Result<(), MediaError> {
        if completed.is_closed() {
            return Err(MediaError::UnknownStream);
        }
        if self.captures.len() + self.workers.len() >= 64 {
            return Err(MediaError::Capacity);
        }
        if self.captures.contains_key(&token)
            || self
                .captures
                .values()
                .any(|capture| capture.target == target)
        {
            return Err(MediaError::InvalidRoute);
        }
        self.captures.insert(
            token,
            Capture {
                target,
                source: Source::Waiting,
                requested: Instant::now(),
                completed,
            },
        );
        Ok(())
    }

    pub fn attach(&mut self, token: MediaTaskToken, sink: RecordingSink) -> Result<(), MediaError> {
        self.attach_at(token, sink, Instant::now())
    }

    fn attach_at(
        &mut self,
        token: MediaTaskToken,
        sink: RecordingSink,
        now: Instant,
    ) -> Result<(), MediaError> {
        let capture = self.captures.get(&token).ok_or(MediaError::UnknownStream)?;
        if matches!(capture.source, Source::Waiting)
            && now.saturating_duration_since(capture.requested) >= Duration::from_secs(5)
        {
            self.expire(token);
            return Err(MediaError::Transport("Recording file timed out".into()));
        }
        let capture = self
            .captures
            .get_mut(&token)
            .ok_or(MediaError::UnknownStream)?;
        match capture.source {
            Source::Waiting => {
                capture.source = Source::Attached(sink);
                Ok(())
            }
            Source::Attached(_) => Err(MediaError::InvalidRoute),
        }
    }

    fn expire(&mut self, token: MediaTaskToken) {
        if let Some(capture) = self.captures.remove(&token) {
            if capture.completed.send(Err(WorkerError::Timeout)).is_err() {
                tracing::debug!(?token, "expired recording receiver disconnected");
            }
        }
    }

    pub fn end(&mut self, token: MediaTaskToken, mode: RecordingEnd) {
        if let Some(capture) = self.captures.remove(&token) {
            let Source::Attached(sink) = capture.source else {
                if capture.completed.send(Ok(())).is_err() {
                    tracing::debug!(?token, "pending recording receiver disconnected");
                }
                return;
            };
            self.workers.spawn(async move {
                let result = match mode {
                    RecordingEnd::Finish => sink.finish().await,
                    RecordingEnd::Cancel => sink.cancel().await,
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

    pub fn release_bridge(&mut self, id: u64) {
        let tokens = self
            .captures
            .iter()
            .filter_map(|(token, capture)| {
                (capture.target == RecordingTarget::Bridge(id)).then_some(*token)
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
                if let Source::Attached(sink) = &mut capture.source {
                    if let Err(error) = sink.try_write(frame) {
                        tracing::warn!(?token, %error, "recording audio refused");
                        failed.push(*token);
                    }
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
        let mut expired = Vec::new();
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
                if let Source::Attached(sink) = &mut capture.source {
                    if let Err(error) = sink.try_write(frame) {
                        tracing::warn!(?token, %error, "bridge recording audio refused");
                        finished.push((*token, RecordingEnd::Finish));
                    }
                }
            }
            if matches!(capture.source, Source::Waiting)
                && capture.requested.elapsed() >= Duration::from_secs(5)
            {
                expired.push(*token);
            }
        }
        for token in expired {
            self.expire(token);
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
