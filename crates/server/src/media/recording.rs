use crate::storage::{RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_core::{MediaTaskToken, RecordingControl, RecordingGate};
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

#[cfg(test)]
mod tests {
    use super::{RecordingEnd, RecordingTarget, Recordings};
    use crate::storage::{StorageWorker, WorkerError};
    use rusterisk_api::ChannelId;
    use rusterisk_core::{MediaTaskKind, MediaTasks, RecordingControl};
    use rusterisk_media::{Frame, MediaError};
    use rusterisk_storage::WaveReader;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn failure(error: impl std::fmt::Display) -> MediaError {
        MediaError::Transport(error.to_string())
    }

    #[tokio::test]
    async fn paused_audio_is_absent_and_input_settings_survive_resumption() -> Result<(), MediaError>
    {
        let directory = tempfile::tempdir().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, completion) = oneshot::channel();
        recordings.reserve(token, RecordingTarget::Bridge(1), completed)?;
        assert_eq!(
            recordings.control(token, RecordingControl::Pause),
            Err(MediaError::InvalidRoute)
        );
        let name = ChannelId::try_from("controlled".to_owned()).map_err(failure)?;
        recordings.attach(token, storage.record(name, 8000).await.map_err(failure)?)?;
        recordings.bridges(&[(1, Frame::new(&[1000; 960], 48000)?)])?;
        storage.recordings().await.map_err(failure)?;
        recordings.control(token, RecordingControl::Pause)?;
        recordings.control(token, RecordingControl::Mute)?;
        recordings.bridges(&[(1, Frame::new(&[9000; 960], 48000)?)])?;
        recordings.control(token, RecordingControl::Resume)?;
        recordings.bridges(&[(1, Frame::new(&[8000; 960], 48000)?)])?;
        storage.recordings().await.map_err(failure)?;
        recordings.control(token, RecordingControl::Unmute)?;
        recordings.bridges(&[(1, Frame::new(&[2000; 960], 48000)?)])?;
        recordings.end(token, RecordingEnd::Finish);
        while let Some(result) = recordings.workers.join_next().await {
            result.map_err(failure)?;
        }
        completion.await.map_err(failure)?.map_err(failure)?;
        let mut reader = WaveReader::open(&directory.path().join("recordings/controlled.wav"))
            .map_err(failure)?;
        for expected in [1000, 0, 2000] {
            let frame = reader
                .read_frame()
                .map_err(failure)?
                .ok_or(MediaError::UnknownStream)?;
            assert_eq!(frame.samples(), [expected; 160]);
        }
        assert!(reader.read_frame().map_err(failure)?.is_none());
        worker.close().await.map_err(failure)?;
        Ok(())
    }

    #[test]
    fn waiting_recordings_expire_at_their_deadline_and_release_their_target(
    ) -> Result<(), MediaError> {
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        recordings.reserve(token, RecordingTarget::Bridge(1), completed)?;
        let deadline = recordings
            .captures
            .get(&token)
            .ok_or(MediaError::UnknownStream)?
            .requested
            + Duration::from_secs(5);
        recordings.bridges_at(&[], deadline - Duration::from_millis(1))?;
        assert!(matches!(
            completion.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        recordings.bridges_at(&[], deadline)?;
        assert!(matches!(
            completion.try_recv().map_err(failure)?,
            Err(WorkerError::Timeout)
        ));
        assert!(tasks.finish(token));
        let replacement = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, _completion) = oneshot::channel();
        recordings.reserve(replacement, RecordingTarget::Bridge(1), completed)?;
        Ok(())
    }

    #[tokio::test]
    async fn recording_attachment_checks_expiry_before_the_next_audio_tick(
    ) -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        recordings.reserve(token, RecordingTarget::Bridge(1), completed)?;
        let deadline = recordings
            .captures
            .get(&token)
            .ok_or(MediaError::UnknownStream)?
            .requested
            + Duration::from_secs(5);
        let name = ChannelId::try_from("expired".to_owned()).map_err(failure)?;
        let sink = storage.record(name.clone(), 8000).await.map_err(failure)?;
        assert!(matches!(
            recordings.attach_at(token, sink, deadline),
            Err(MediaError::Transport(_))
        ));
        assert!(matches!(
            completion.try_recv().map_err(failure)?,
            Err(WorkerError::Timeout)
        ));
        let replacement = storage.record(name, 8000).await.map_err(failure)?;
        replacement.cancel().await.map_err(failure)?;
        worker.close().await.map_err(failure)?;
        Ok(())
    }
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
    Attached {
        sink: RecordingSink,
        gate: RecordingGate,
    },
}

impl Capture {
    fn write(&mut self, frame: Frame) -> Result<(), WorkerError> {
        match &mut self.source {
            Source::Waiting => Ok(()),
            Source::Attached { sink, gate } => match gate {
                RecordingGate::Open => sink.try_write(frame),
                RecordingGate::Silent => {
                    let silence = Frame::new(
                        &[0; rusterisk_media::MAX_SAMPLES][..frame.samples().len()],
                        frame.rate(),
                    )
                    .map_err(|error| WorkerError::Storage(error.into()))?;
                    sink.try_write(silence)
                }
                RecordingGate::Paused | RecordingGate::PausedSilent => Ok(()),
            },
        }
    }
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
                capture.source = Source::Attached {
                    sink,
                    gate: RecordingGate::Open,
                };
                Ok(())
            }
            Source::Attached { .. } => Err(MediaError::InvalidRoute),
        }
    }

    fn expire(&mut self, token: MediaTaskToken) {
        if let Some(capture) = self.captures.remove(&token) {
            if capture.completed.send(Err(WorkerError::Timeout)).is_err() {
                tracing::debug!(?token, "expired recording receiver disconnected");
            }
        }
    }

    pub fn control(
        &mut self,
        token: MediaTaskToken,
        operation: RecordingControl,
    ) -> Result<(), MediaError> {
        let capture = self
            .captures
            .get_mut(&token)
            .ok_or(MediaError::UnknownStream)?;
        match &mut capture.source {
            Source::Waiting => Err(MediaError::InvalidRoute),
            Source::Attached { gate, .. } => {
                *gate = gate.apply(operation);
                Ok(())
            }
        }
    }

    pub fn end(&mut self, token: MediaTaskToken, mode: RecordingEnd) {
        if let Some(capture) = self.captures.remove(&token) {
            let Source::Attached { sink, .. } = capture.source else {
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
                if let Err(error) = capture.write(frame) {
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
        self.bridges_at(frames, Instant::now())
    }

    fn bridges_at(&mut self, frames: &[(u64, Frame)], now: Instant) -> Result<(), MediaError> {
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
                if let Err(error) = capture.write(frame) {
                    tracing::warn!(?token, %error, "bridge recording audio refused");
                    finished.push((*token, RecordingEnd::Finish));
                }
            }
            if matches!(capture.source, Source::Waiting)
                && now.saturating_duration_since(capture.requested) >= Duration::from_secs(5)
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
