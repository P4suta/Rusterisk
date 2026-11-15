use crate::storage::PlaybackSource;
use crate::CallKey;
use rusterisk_core::MediaTaskToken;
use rusterisk_media::{AudioTarget, Frame, MediaError};
use std::collections::BTreeMap;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackControl {
    Pause,
    Resume,
    Stop,
}

enum State {
    Active,
    Paused,
}

enum Source {
    Waiting,
    Attached {
        source: PlaybackSource,
        state: State,
    },
}

struct Playback {
    target: AudioTarget<CallKey>,
    source: Source,
    completed: oneshot::Sender<Result<(), MediaError>>,
    progress: Instant,
}

pub(super) struct Playbacks {
    entries: BTreeMap<MediaTaskToken, Playback>,
}

impl Playbacks {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn reserve(
        &mut self,
        token: MediaTaskToken,
        target: AudioTarget<CallKey>,
        completed: oneshot::Sender<Result<(), MediaError>>,
    ) -> Result<(), MediaError> {
        if completed.is_closed() {
            return Err(MediaError::UnknownStream);
        }
        if self.entries.len() >= 64 {
            return Err(MediaError::Capacity);
        }
        if self.entries.contains_key(&token)
            || self.entries.values().any(|entry| entry.target == target)
        {
            return Err(MediaError::InvalidRoute);
        }
        self.entries.insert(
            token,
            Playback {
                target,
                source: Source::Waiting,
                completed,
                progress: Instant::now(),
            },
        );
        Ok(())
    }

    pub fn attach(
        &mut self,
        token: MediaTaskToken,
        source: PlaybackSource,
    ) -> Result<(), MediaError> {
        self.attach_at(token, source, Instant::now())
    }

    fn attach_at(
        &mut self,
        token: MediaTaskToken,
        source: PlaybackSource,
        now: Instant,
    ) -> Result<(), MediaError> {
        let entry = self.entries.get(&token).ok_or(MediaError::UnknownStream)?;
        if matches!(entry.source, Source::Waiting)
            && now.saturating_duration_since(entry.progress) >= Duration::from_secs(5)
        {
            let error = MediaError::Transport("Playback source timed out".into());
            self.end(token, Err(error.clone()));
            return Err(error);
        }
        let entry = self
            .entries
            .get_mut(&token)
            .ok_or(MediaError::UnknownStream)?;
        match entry.source {
            Source::Waiting => {
                entry.source = Source::Attached {
                    source,
                    state: State::Active,
                };
                entry.progress = now;
                Ok(())
            }
            Source::Attached { .. } => Err(MediaError::InvalidRoute),
        }
    }

    pub fn end(&mut self, token: MediaTaskToken, result: Result<(), MediaError>) {
        if let Some(entry) = self.entries.remove(&token) {
            drop(entry.source);
            if let Err(error) = &result {
                tracing::warn!(?token, %error, "playback ended with an error");
            }
            if entry.completed.send(result).is_err() {
                tracing::debug!(?token, "playback receiver disconnected");
            }
        }
    }

    pub fn control(&mut self, token: MediaTaskToken, operation: PlaybackControl) {
        match operation {
            PlaybackControl::Stop => self.end(token, Ok(())),
            PlaybackControl::Pause | PlaybackControl::Resume => {
                if let Some(entry) = self.entries.get_mut(&token) {
                    if let Source::Attached { state, .. } = &mut entry.source {
                        *state = match operation {
                            PlaybackControl::Pause => State::Paused,
                            PlaybackControl::Resume => State::Active,
                            PlaybackControl::Stop => return,
                        };
                        entry.progress = Instant::now();
                    }
                }
            }
        }
    }

    pub fn release(&mut self, key: &CallKey) {
        let tokens = self
            .entries
            .iter()
            .filter_map(|(token, entry)| match &entry.target {
                AudioTarget::Channel(channel) => (channel == key).then_some(*token),
                AudioTarget::Bridge(_) => None,
            })
            .collect::<Vec<_>>();
        for token in tokens {
            self.end(token, Ok(()));
        }
    }

    pub fn tick(&mut self) -> Result<Vec<(AudioTarget<CallKey>, Frame)>, MediaError> {
        self.tick_at(Instant::now())
    }

    fn tick_at(&mut self, now: Instant) -> Result<Vec<(AudioTarget<CallKey>, Frame)>, MediaError> {
        let mut frames = Vec::with_capacity(self.entries.len());
        let mut finished = Vec::new();
        for (token, entry) in &mut self.entries {
            if entry.completed.is_closed() {
                finished.push((*token, Ok(())));
                continue;
            }
            let source = match &mut entry.source {
                Source::Waiting => {
                    if now.saturating_duration_since(entry.progress) >= Duration::from_secs(5) {
                        finished.push((
                            *token,
                            Err(MediaError::Transport("Playback source timed out".into())),
                        ));
                    }
                    continue;
                }
                Source::Attached {
                    state: State::Paused,
                    ..
                } => continue,
                Source::Attached {
                    source,
                    state: State::Active,
                } => source,
            };
            match source.try_read() {
                Ok(Poll::Ready(Some(frame))) => {
                    let frame = frame.resample(48000)?;
                    let mut samples = [0; 960];
                    samples[..frame.samples().len()].copy_from_slice(frame.samples());
                    frames.push((entry.target.clone(), Frame::new(&samples, 48000)?));
                    entry.progress = now;
                }
                Ok(Poll::Ready(None)) => finished.push((*token, Ok(()))),
                Ok(Poll::Pending) => {
                    if now.saturating_duration_since(entry.progress) >= Duration::from_secs(5) {
                        finished.push((
                            *token,
                            Err(MediaError::Transport("Playback audio timed out".into())),
                        ));
                    }
                }
                Err(error) => {
                    finished.push((*token, Err(MediaError::Transport(error.to_string()))))
                }
            }
        }
        for (token, result) in finished {
            self.end(token, result);
        }
        Ok(frames)
    }

    pub fn close(&mut self) {
        let tokens = self.entries.keys().copied().collect::<Vec<_>>();
        for token in tokens {
            self.end(token, Ok(()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackControl, Playbacks};
    use crate::storage::{PlaybackMedia, StorageWorker};
    use rusterisk_api::ChannelId;
    use rusterisk_core::{MediaTaskKind, MediaTasks};
    use rusterisk_media::{AudioTarget, Frame, MediaError};
    use rusterisk_storage::Recorder;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn failure(error: impl std::fmt::Display) -> MediaError {
        MediaError::Transport(error.to_string())
    }

    #[test]
    fn waiting_sources_expire_at_their_deadline_and_release_their_target() -> Result<(), MediaError>
    {
        let mut playbacks = Playbacks::new();
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        playbacks.reserve(token, AudioTarget::Bridge(1), completed)?;
        let deadline = playbacks
            .entries
            .get(&token)
            .ok_or(MediaError::UnknownStream)?
            .progress
            + Duration::from_secs(5);
        assert!(playbacks
            .tick_at(deadline - Duration::from_millis(1))?
            .is_empty());
        assert!(matches!(
            completion.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(playbacks.tick_at(deadline)?.is_empty());
        assert!(matches!(
            completion.try_recv().map_err(failure)?,
            Err(MediaError::Transport(_))
        ));
        assert!(tasks.finish(token));
        let replacement = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, _completion) = oneshot::channel();
        playbacks.reserve(replacement, AudioTarget::Bridge(1), completed)?;
        Ok(())
    }

    #[test]
    fn releasing_a_channel_only_removes_its_own_generation() -> Result<(), MediaError> {
        let mut playbacks = Playbacks::new();
        let old = crate::CallKey {
            id: ChannelId::try_from("phone".to_owned()).map_err(failure)?,
            generation: 1,
        };
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        playbacks.reserve(token, AudioTarget::Channel(old.clone()), completed)?;
        playbacks.release(&old);
        completion.try_recv().map_err(failure)??;
        let current = crate::CallKey {
            id: old.id.clone(),
            generation: 2,
        };
        let mut tasks = MediaTasks::new(2);
        let token = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let (completed, mut completion) = oneshot::channel();
        playbacks.reserve(token, AudioTarget::Channel(current.clone()), completed)?;
        playbacks.release(&old);
        assert!(matches!(
            completion.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        playbacks.release(&current);
        completion.try_recv().map_err(failure)??;
        Ok(())
    }

    #[tokio::test]
    async fn attachment_checks_expiry_before_the_next_audio_tick() -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        Recorder::create(directory.path(), "expired", 8000)
            .map_err(failure)?
            .finish()
            .map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let source = storage
            .play(PlaybackMedia::Recording(
                ChannelId::try_from("expired".to_owned()).map_err(failure)?,
            ))
            .await
            .map_err(failure)?;
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let mut playbacks = Playbacks::new();
        let (completed, mut completion) = oneshot::channel();
        playbacks.reserve(token, AudioTarget::Bridge(1), completed)?;
        let deadline = playbacks
            .entries
            .get(&token)
            .ok_or(MediaError::UnknownStream)?
            .progress
            + Duration::from_secs(5);
        assert!(matches!(
            playbacks.attach_at(token, source, deadline),
            Err(MediaError::Transport(_))
        ));
        assert!(matches!(
            completion.try_recv().map_err(failure)?,
            Err(MediaError::Transport(_))
        ));
        assert!(playbacks.entries.is_empty());
        worker.close().await.map_err(failure)?;
        Ok(())
    }

    #[tokio::test]
    async fn paused_playback_keeps_its_next_sample_and_stopping_releases_the_source(
    ) -> Result<(), MediaError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let mut recording = Recorder::create(directory.path(), "ordered", 8000).map_err(failure)?;
        for sample in [1000, 2000, 3000, 4000] {
            recording
                .write(Frame::new(&[sample; 160], 8000)?)
                .map_err(failure)?;
        }
        recording.finish().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let source = storage
            .play(PlaybackMedia::Recording(
                ChannelId::try_from("ordered".to_owned()).map_err(failure)?,
            ))
            .await
            .map_err(failure)?;
        storage.recordings().await.map_err(failure)?;
        let mut tasks = MediaTasks::new(1);
        let token = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let mut playbacks = Playbacks::new();
        let (completed, completion) = oneshot::channel();
        playbacks.reserve(token, AudioTarget::Bridge(1), completed)?;
        playbacks.attach(token, source)?;
        assert_eq!(playbacks.tick()?[0].1.samples(), &[1000; 960]);
        playbacks.control(token, PlaybackControl::Pause);
        for _ in 0..100 {
            assert!(playbacks.tick()?.is_empty());
        }
        playbacks.control(token, PlaybackControl::Resume);
        assert_eq!(playbacks.tick()?[0].1.samples(), &[2000; 960]);
        playbacks.control(token, PlaybackControl::Stop);
        completion.await.map_err(failure)??;
        assert!(playbacks.tick()?.is_empty());
        storage.recordings().await.map_err(failure)?;
        worker.close().await.map_err(failure)?;
        Ok(())
    }
}
