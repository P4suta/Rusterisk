use crate::storage::PlaybackSource;
use crate::CallKey;
use rusterisk_core::MediaTaskToken;
use rusterisk_media::{AudioTarget, Frame, MediaError};
use std::collections::BTreeMap;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

struct Playback {
    target: AudioTarget<CallKey>,
    source: PlaybackSource,
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
