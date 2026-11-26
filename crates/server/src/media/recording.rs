use crate::storage::{RecordingSink, WorkerError};
use crate::CallKey;
use rusterisk_api::{RecordingOptions, RecordingTermination};
use rusterisk_core::{
    DtmfDigit, MediaTaskToken, RecordingClock, RecordingControl, RecordingGate, RecordingSilence,
    RecordingStopKey,
};
use rusterisk_media::{Frame, MediaError};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;

pub type RecordingPrompt = oneshot::Receiver<Result<(), MediaError>>;

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
    use tokio::time::Instant;

    fn failure(error: impl std::fmt::Display) -> MediaError {
        MediaError::Transport(error.to_string())
    }

    #[tokio::test]
    async fn recording_prompts_exclude_early_input_and_release_their_completion_receiver_on_cancel(
    ) -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let key = crate::CallKey {
            id: ChannelId::try_from("phone".to_owned()).map_err(failure)?,
            generation: 1,
        };
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        for name in ["completed", "failed", "canceled"] {
            let token = tasks
                .reserve(MediaTaskKind::Recording)
                .map_err(|_| MediaError::Capacity)?;
            let (completed, completion) = oneshot::channel();
            recordings.reserve(token, RecordingTarget::Channel(key.clone()), completed)?;
            let sink = storage
                .record(ChannelId::try_from(name.to_owned()).map_err(failure)?, 8000)
                .await
                .map_err(failure)?;
            let (prompt, receiver) = oneshot::channel();
            let start = Instant::now();
            let options = rusterisk_api::RecordingOptions {
                beep: true,
                max_duration_seconds: rusterisk_api::RecordingSeconds::try_from(1)
                    .map_err(failure)?,
                ..Default::default()
            };
            recordings.attach_options_at(token, sink, options, Some(receiver), start)?;
            recordings.channel(&key, Frame::new(&[8000; 160], 8000)?);
            recordings.bridges_at(&[], start + Duration::from_secs(10))?;
            assert_eq!(recordings.captures.len(), 1);
            if name == "canceled" {
                recordings.end(token, RecordingEnd::Cancel);
                assert!(prompt.is_closed());
            } else if name == "failed" {
                prompt
                    .send(Err(MediaError::InvalidFrame))
                    .map_err(|_| MediaError::InvalidRoute)?;
                recordings.bridges_at(&[], start + Duration::from_secs(10))?;
            } else {
                recordings.control_at(
                    token,
                    RecordingControl::Pause,
                    start + Duration::from_secs(10),
                )?;
                prompt.send(Ok(())).map_err(|_| MediaError::InvalidRoute)?;
                recordings.bridges_at(&[], start + Duration::from_secs(10))?;
                recordings.channel(&key, Frame::new(&[9000; 160], 8000)?);
                recordings.control_at(
                    token,
                    RecordingControl::Resume,
                    start + Duration::from_secs(10),
                )?;
                recordings.channel(&key, Frame::new(&[1234; 160], 8000)?);
                recordings.end(token, RecordingEnd::Finish);
            }
            while let Some(result) = recordings.workers.join_next().await {
                result.map_err(failure)?;
            }
            completion.await.map_err(failure)?.map_err(failure)?;
            let path = directory.path().join(format!("recordings/{name}.wav"));
            if name == "completed" {
                let mut reader = WaveReader::open(&path).map_err(failure)?;
                assert_eq!(
                    reader
                        .read_frame()
                        .map_err(failure)?
                        .ok_or(MediaError::InvalidFrame)?
                        .samples(),
                    [1234; 160]
                );
                assert!(reader.read_frame().map_err(failure)?.is_none());
            } else {
                assert!(!path.exists());
            }
            assert!(tasks.finish(token));
        }
        worker.close().await.map_err(failure)?;
        Ok(())
    }

    #[tokio::test]
    async fn silence_limits_detect_original_input_and_keep_only_two_hundred_milliseconds_of_the_tail(
    ) -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let key = crate::CallKey {
            id: ChannelId::try_from("phone".to_owned()).map_err(failure)?,
            generation: 1,
        };
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        for muted in [false, true] {
            let name = if muted { "muted" } else { "quiet" };
            let token = tasks
                .reserve(MediaTaskKind::Recording)
                .map_err(|_| MediaError::Capacity)?;
            let (completed, completion) = oneshot::channel();
            recordings.reserve(token, RecordingTarget::Channel(key.clone()), completed)?;
            let sink = storage
                .record(ChannelId::try_from(name.to_owned()).map_err(failure)?, 8000)
                .await
                .map_err(failure)?;
            recordings.attach(
                token,
                sink,
                rusterisk_api::RecordingOptions {
                    max_silence_seconds: rusterisk_api::RecordingSeconds::try_from(1)
                        .map_err(failure)?,
                    ..Default::default()
                },
                None,
            )?;
            if muted {
                recordings.control(token, RecordingControl::Mute)?;
            }
            for index in 0..if muted { 60 } else { 56 } {
                let sample = if muted || index < 5 { 128 } else { 127 };
                recordings.channel(&key, Frame::new(&[sample; 160], 8000)?);
                storage.recordings().await.map_err(failure)?;
            }
            if muted {
                assert_eq!(recordings.captures.len(), 1);
                recordings.end(token, RecordingEnd::Finish);
            } else {
                assert!(recordings.captures.is_empty());
            }
            while let Some(result) = recordings.workers.join_next().await {
                result.map_err(failure)?;
            }
            completion.await.map_err(failure)?.map_err(failure)?;
            let mut reader =
                WaveReader::open(&directory.path().join(format!("recordings/{name}.wav")))
                    .map_err(failure)?;
            let expected = if muted { 60 } else { 15 };
            for index in 0..expected {
                let sample = if muted {
                    0
                } else if index < 5 {
                    128
                } else {
                    127
                };
                assert_eq!(
                    reader
                        .read_frame()
                        .map_err(failure)?
                        .ok_or(MediaError::InvalidFrame)?
                        .samples(),
                    [sample; 160]
                );
            }
            assert!(reader.read_frame().map_err(failure)?.is_none());
            assert!(tasks.finish(token));
        }
        worker.close().await.map_err(failure)?;
        Ok(())
    }

    #[tokio::test]
    async fn stop_keys_finish_only_matching_channel_generations_and_bridge_members(
    ) -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let key = crate::CallKey {
            id: ChannelId::try_from("phone".to_owned()).map_err(failure)?,
            generation: 1,
        };
        let mut recordings = Recordings::new();
        let mut completions = Vec::new();
        for (generation, target, name, stop_key) in [
            (
                1,
                RecordingTarget::Channel(key.clone()),
                "channel",
                rusterisk_api::RecordingTermination::Hash,
            ),
            (
                2,
                RecordingTarget::Bridge(2),
                "bridge",
                rusterisk_api::RecordingTermination::Any,
            ),
        ] {
            let mut tasks = MediaTasks::new(generation);
            let token = tasks
                .reserve(MediaTaskKind::Recording)
                .map_err(|_| MediaError::Capacity)?;
            let (completed, completion) = oneshot::channel();
            recordings.reserve(token, target, completed)?;
            let sink = storage
                .record(ChannelId::try_from(name.to_owned()).map_err(failure)?, 8000)
                .await
                .map_err(failure)?;
            recordings.attach(
                token,
                sink,
                rusterisk_api::RecordingOptions {
                    terminate_on: stop_key,
                    ..Default::default()
                },
                None,
            )?;
            recordings.control(token, RecordingControl::Pause)?;
            completions.push(completion);
        }
        recordings.received_digit(
            &crate::CallKey {
                generation: 3,
                ..key.clone()
            },
            None,
            rusterisk_core::DtmfDigit::Hash,
        );
        recordings.received_digit(&key, Some(3), rusterisk_core::DtmfDigit::One);
        assert_eq!(recordings.captures.len(), 2);
        recordings.received_digit(&key, Some(2), rusterisk_core::DtmfDigit::One);
        assert_eq!(recordings.captures.len(), 1);
        recordings.received_digit(&key, None, rusterisk_core::DtmfDigit::Hash);
        assert!(recordings.captures.is_empty());
        while let Some(result) = recordings.workers.join_next().await {
            result.map_err(failure)?;
        }
        for completed in completions {
            completed.await.map_err(failure)?.map_err(failure)?;
        }
        assert_eq!(storage.recordings().await.map_err(failure)?.len(), 2);
        worker.close().await.map_err(failure)?;
        Ok(())
    }

    #[tokio::test]
    async fn duration_limits_start_after_file_attachment_and_ignore_paused_time(
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
        let sink = storage
            .record(
                ChannelId::try_from("limited".to_owned()).map_err(failure)?,
                8000,
            )
            .await
            .map_err(failure)?;
        let start = Instant::now();
        let options = rusterisk_api::RecordingOptions {
            max_duration_seconds: rusterisk_api::RecordingSeconds::try_from(1).map_err(failure)?,
            ..Default::default()
        };
        recordings.attach_options_at(token, sink, options, None, start)?;
        recordings.bridges_at(&[], start + Duration::from_millis(1999))?;
        recordings.control_at(
            token,
            RecordingControl::Pause,
            start + Duration::from_millis(1999),
        )?;
        recordings.bridges_at(&[], start + Duration::from_secs(10))?;
        assert!(matches!(
            completion.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        recordings.control_at(
            token,
            RecordingControl::Resume,
            start + Duration::from_secs(10),
        )?;
        recordings.bridges_at(&[], start + Duration::from_millis(10001))?;
        while let Some(result) = recordings.workers.join_next().await {
            result.map_err(failure)?;
        }
        completion.await.map_err(failure)?.map_err(failure)?;
        assert!(recordings.captures.is_empty());
        assert!(directory.path().join("recordings/limited.wav").exists());
        worker.close().await.map_err(failure)?;
        Ok(())
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
        recordings.attach(
            token,
            storage.record(name, 8000).await.map_err(failure)?,
            Default::default(),
            None,
        )?;
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

    // A bridge recording that waits for its file ends after five seconds of runtime time.
    #[tokio::test(start_paused = true)]
    async fn a_waiting_recording_ends_after_five_seconds_of_runtime_time() -> Result<(), MediaError>
    {
        let mut recordings = Recordings::new();
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        recordings.reserve(token, RecordingTarget::Bridge(1), completed)?;
        tokio::time::advance(Duration::from_secs(5)).await;
        recordings.bridges(&[])?;
        assert!(matches!(
            completion.try_recv().map_err(failure)?,
            Err(WorkerError::Timeout)
        ));
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
            recordings.attach_options_at(token, sink, Default::default(), None, deadline),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingEnd {
    Finish,
    Cancel,
}

enum Finalization {
    Finish,
    Trimmed(u64),
    Cancel,
}

enum RecordingWrite {
    Continue,
    SilenceLimit,
}

enum RecordingTick {
    Continue,
    DurationLimit,
    PromptFailed,
}

enum Stage {
    Prompt(RecordingPrompt),
    Capturing,
}

struct Capture {
    target: RecordingTarget,
    source: Source,
    requested: Instant,
    silence: RecordingSilence,
    completed: oneshot::Sender<Result<(), WorkerError>>,
}

enum Source {
    Waiting,
    Attached {
        sink: RecordingSink,
        gate: RecordingGate,
        clock: RecordingClock,
        updated: Instant,
        stop_key: RecordingStopKey,
        stage: Stage,
    },
}

impl Capture {
    fn after_limit(&self) -> Finalization {
        Finalization::Trimmed(self.silence.trailing_milliseconds().saturating_sub(200))
    }

    fn tick(&mut self, now: Instant) -> RecordingTick {
        match &mut self.source {
            Source::Waiting => RecordingTick::Continue,
            Source::Attached {
                gate,
                clock,
                updated,
                stage,
                ..
            } => {
                let now = now.max(*updated);
                if let Stage::Prompt(completed) = stage {
                    match completed.try_recv() {
                        Ok(Ok(())) => {
                            *stage = Stage::Capturing;
                            *updated = now;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "recording prompt failed");
                            return RecordingTick::PromptFailed;
                        }
                        Err(oneshot::error::TryRecvError::Closed) => {
                            return RecordingTick::PromptFailed
                        }
                        Err(oneshot::error::TryRecvError::Empty) => return RecordingTick::Continue,
                    }
                }
                let expired = match gate {
                    RecordingGate::Paused | RecordingGate::PausedSilent => {
                        *updated = now;
                        clock.advance(0, *gate)
                    }
                    RecordingGate::Open | RecordingGate::Silent => {
                        let elapsed = u64::try_from(now.duration_since(*updated).as_millis())
                            .unwrap_or(u64::MAX);
                        *updated += Duration::from_millis(elapsed);
                        clock.advance(elapsed, *gate)
                    }
                };
                if expired {
                    RecordingTick::DurationLimit
                } else {
                    RecordingTick::Continue
                }
            }
        }
    }

    fn write(&mut self, frame: Frame) -> Result<RecordingWrite, WorkerError> {
        match &mut self.source {
            Source::Waiting => Ok(RecordingWrite::Continue),
            Source::Attached {
                stage: Stage::Prompt(_),
                ..
            } => Ok(RecordingWrite::Continue),
            Source::Attached {
                sink,
                gate,
                stage: Stage::Capturing,
                ..
            } => {
                let frame = frame
                    .resample(8000)
                    .map_err(|error| WorkerError::Storage(error.into()))?;
                let magnitude = frame
                    .samples()
                    .iter()
                    .map(|sample| u64::from(sample.unsigned_abs()))
                    .sum::<u64>()
                    / frame.samples().len() as u64;
                let milliseconds = frame.samples().len() as u64 * 1000 / u64::from(frame.rate());
                match gate {
                    RecordingGate::Open => sink.try_write(frame)?,
                    RecordingGate::Silent => {
                        let silence = Frame::new(
                            &[0; rusterisk_media::MAX_SAMPLES][..frame.samples().len()],
                            frame.rate(),
                        )
                        .map_err(|error| WorkerError::Storage(error.into()))?;
                        sink.try_write(silence)?;
                    }
                    RecordingGate::Paused | RecordingGate::PausedSilent => {}
                }
                Ok(
                    if self.silence.observe(milliseconds, magnitude < 128, *gate) {
                        RecordingWrite::SilenceLimit
                    } else {
                        RecordingWrite::Continue
                    },
                )
            }
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

    pub fn needs_frames(&self) -> bool {
        !self.captures.is_empty()
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
                silence: RecordingSilence::new(0),
                completed,
            },
        );
        Ok(())
    }

    pub fn attach(
        &mut self,
        token: MediaTaskToken,
        sink: RecordingSink,
        options: RecordingOptions,
        prompt: Option<RecordingPrompt>,
    ) -> Result<(), MediaError> {
        self.attach_options_at(token, sink, options, prompt, Instant::now())
    }

    fn attach_options_at(
        &mut self,
        token: MediaTaskToken,
        sink: RecordingSink,
        options: RecordingOptions,
        prompt: Option<RecordingPrompt>,
        now: Instant,
    ) -> Result<(), MediaError> {
        if options.beep != prompt.is_some() {
            return Err(MediaError::InvalidRoute);
        }
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
                capture.silence = RecordingSilence::new(options.max_silence_seconds.seconds());
                capture.source = Source::Attached {
                    sink,
                    stage: match prompt {
                        Some(completed) => Stage::Prompt(completed),
                        None => Stage::Capturing,
                    },
                    gate: RecordingGate::Open,
                    clock: RecordingClock::new(options.max_duration_seconds.seconds()),
                    updated: now,
                    stop_key: match options.terminate_on {
                        RecordingTermination::None => RecordingStopKey::None,
                        RecordingTermination::Any => RecordingStopKey::Any,
                        RecordingTermination::Star => RecordingStopKey::Star,
                        RecordingTermination::Hash => RecordingStopKey::Hash,
                    },
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
        self.control_at(token, operation, Instant::now())
    }

    fn control_at(
        &mut self,
        token: MediaTaskToken,
        operation: RecordingControl,
        now: Instant,
    ) -> Result<(), MediaError> {
        let capture = self
            .captures
            .get_mut(&token)
            .ok_or(MediaError::UnknownStream)?;
        match capture.tick(now) {
            RecordingTick::Continue => {}
            RecordingTick::DurationLimit => {
                let mode = capture.after_limit();
                self.finalize(token, mode);
                return Err(MediaError::UnknownStream);
            }
            RecordingTick::PromptFailed => {
                self.finalize(token, Finalization::Cancel);
                return Err(MediaError::UnknownStream);
            }
        }
        match &mut capture.source {
            Source::Waiting => Err(MediaError::InvalidRoute),
            Source::Attached { gate, .. } => {
                *gate = gate.apply(operation);
                Ok(())
            }
        }
    }

    pub fn received_digit(&mut self, key: &CallKey, bridge: Option<u64>, digit: DtmfDigit) {
        let completed = self
            .captures
            .iter()
            .filter_map(|(token, capture)| {
                let accepts = match &capture.source {
                    Source::Waiting => false,
                    Source::Attached {
                        stage: Stage::Prompt(_),
                        ..
                    } => false,
                    Source::Attached {
                        stop_key,
                        stage: Stage::Capturing,
                        ..
                    } => stop_key.accepts(digit),
                };
                let matches = match &capture.target {
                    RecordingTarget::Channel(channel) => channel == key,
                    RecordingTarget::Bridge(generation) => Some(*generation) == bridge,
                };
                (accepts && matches).then(|| (*token, capture.after_limit()))
            })
            .collect::<Vec<_>>();
        for (token, mode) in completed {
            self.finalize(token, mode);
        }
    }

    pub fn end(&mut self, token: MediaTaskToken, mode: RecordingEnd) {
        self.finalize(
            token,
            match mode {
                RecordingEnd::Finish => Finalization::Finish,
                RecordingEnd::Cancel => Finalization::Cancel,
            },
        );
    }

    fn finalize(&mut self, token: MediaTaskToken, mode: Finalization) {
        if let Some(capture) = self.captures.remove(&token) {
            let Source::Attached { sink, stage, .. } = capture.source else {
                if capture.completed.send(Ok(())).is_err() {
                    tracing::debug!(?token, "pending recording receiver disconnected");
                }
                return;
            };
            drop(stage);
            self.workers.spawn(async move {
                let result = match mode {
                    Finalization::Finish => sink.finish().await,
                    Finalization::Trimmed(milliseconds) => {
                        sink.finish_with_tail_trim(milliseconds).await
                    }
                    Finalization::Cancel => sink.cancel().await,
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
        let mut finished = Vec::new();
        for (token, capture) in &mut self.captures {
            if capture.target == RecordingTarget::Channel(key.clone()) {
                match capture.write(frame) {
                    Ok(RecordingWrite::Continue) => {}
                    Ok(RecordingWrite::SilenceLimit) => {
                        finished.push((*token, capture.after_limit()))
                    }
                    Err(error) => {
                        tracing::warn!(?token, %error, "recording audio refused");
                        finished.push((*token, Finalization::Finish));
                    }
                }
            }
        }
        for (token, mode) in finished {
            self.finalize(token, mode);
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
                finished.push((*token, Finalization::Cancel));
                continue;
            }
            match capture.tick(now) {
                RecordingTick::Continue => {}
                RecordingTick::DurationLimit => {
                    finished.push((*token, capture.after_limit()));
                    continue;
                }
                RecordingTick::PromptFailed => {
                    finished.push((*token, Finalization::Cancel));
                    continue;
                }
            }
            if let RecordingTarget::Bridge(id) = capture.target {
                let frame = frames
                    .iter()
                    .find_map(|(bridge, frame)| (*bridge == id).then_some(*frame))
                    .unwrap_or(silence);
                match capture.write(frame) {
                    Ok(RecordingWrite::Continue) => {}
                    Ok(RecordingWrite::SilenceLimit) => {
                        finished.push((*token, capture.after_limit()))
                    }
                    Err(error) => {
                        tracing::warn!(?token, %error, "bridge recording audio refused");
                        finished.push((*token, Finalization::Finish));
                    }
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
            self.finalize(token, mode);
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
