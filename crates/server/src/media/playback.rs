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

struct Playback {
    target: AudioTarget<CallKey>,
    source: PlaybackSource,
    completed: oneshot::Sender<Result<(), MediaError>>,
    progress: Instant,
    state: State,
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

    pub fn begin(
        &mut self,
        token: MediaTaskToken,
        target: AudioTarget<CallKey>,
        source: PlaybackSource,
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
                source,
                completed,
                progress: Instant::now(),
                state: State::Active,
            },
        );
        Ok(())
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
                    entry.state = match operation {
                        PlaybackControl::Pause => State::Paused,
                        PlaybackControl::Resume => State::Active,
                        PlaybackControl::Stop => return,
                    };
                    entry.progress = Instant::now();
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
        let mut frames = Vec::with_capacity(self.entries.len());
        let mut finished = Vec::new();
        for (token, entry) in &mut self.entries {
            if entry.completed.is_closed() {
                finished.push((*token, Ok(())));
                continue;
            }
            if matches!(entry.state, State::Paused) {
                continue;
            }
            match entry.source.try_read() {
                Ok(Poll::Ready(Some(frame))) => {
                    let frame = frame.resample(48000)?;
                    let mut samples = [0; 960];
                    samples[..frame.samples().len()].copy_from_slice(frame.samples());
                    frames.push((entry.target.clone(), Frame::new(&samples, 48000)?));
                    entry.progress = Instant::now();
                }
                Ok(Poll::Ready(None)) => finished.push((*token, Ok(()))),
                Ok(Poll::Pending) => {
                    if entry.progress.elapsed() >= Duration::from_secs(5) {
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
    use tokio::sync::oneshot;

    fn failure(error: impl std::fmt::Display) -> MediaError {
        MediaError::Transport(error.to_string())
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
        playbacks.begin(token, AudioTarget::Bridge(1), source, completed)?;
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
