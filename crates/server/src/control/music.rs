use super::{CallKey, Control, ControlError, Effect, MediaOwner, Outcome, StasisMembership};
use crate::media::PlaybackControl;
use crate::storage::{MusicSelection, PreparedMusic};
use rusterisk_api::{ChannelId, ChannelState};
use rusterisk_core::{CallState, MediaState, MediaTaskKind, MediaTaskToken};

pub(super) enum MusicTask {
    Selected {
        token: MediaTaskToken,
        class: Option<String>,
        selection: Result<MusicSelection, ControlError>,
    },
    Preparing {
        token: MediaTaskToken,
        class: Option<String>,
        selection: Result<MusicSelection, ControlError>,
    },
    Running {
        token: MediaTaskToken,
        class: Option<String>,
    },
}

impl MusicTask {
    pub(super) fn token(&self) -> MediaTaskToken {
        match self {
            Self::Selected { token, .. }
            | Self::Preparing { token, .. }
            | Self::Running { token, .. } => *token,
        }
    }

    fn class(&self) -> Option<&str> {
        match self {
            Self::Selected { class, .. }
            | Self::Preparing { class, .. }
            | Self::Running { class, .. } => class.as_deref(),
        }
    }
}

impl Control {
    pub(crate) fn with_music(mut self, catalog: PreparedMusic) -> Result<Self, ControlError> {
        let mut settings = self.settings.get(self.settings.current())?.clone();
        settings.music_catalog = Some(catalog);
        self.publish_call_settings(settings)?;
        Ok(self)
    }

    pub(super) fn music_source_count(&self) -> usize {
        self.playbacks.len()
            + self
                .channels
                .values()
                .filter(|entry| entry.music.is_some())
                .count()
    }

    pub(super) fn start_music(
        &mut self,
        channel: ChannelId,
        requested: Option<String>,
    ) -> Result<Outcome, ControlError> {
        let catalog = self
            .channel_settings(&channel)?
            .music_catalog
            .as_ref()
            .ok_or(ControlError::Unsupported)?;
        let selection = requested
            .as_deref()
            .filter(|name| !name.is_empty())
            .and_then(|name| catalog.select(name))
            .or_else(|| catalog.select("default"))
            .ok_or(ControlError::Invalid("Music class unavailable"));
        self.start_selected_music(channel, selection)
    }

    pub(super) fn start_default_music(
        &mut self,
        channel: ChannelId,
    ) -> Result<Outcome, ControlError> {
        match self
            .channel_settings(&channel)?
            .music_catalog
            .as_ref()
            .and_then(|catalog| catalog.select("default"))
        {
            Some(selection) => self.start_selected_music(channel, Ok(selection)),
            None => Ok(Outcome::empty()),
        }
    }

    fn start_selected_music(
        &mut self,
        channel: ChannelId,
        selection: Result<MusicSelection, ControlError>,
    ) -> Result<Outcome, ControlError> {
        let class = selection
            .as_ref()
            .ok()
            .map(|selection| selection.class_name().to_owned());
        let entry = self
            .channels
            .get(&channel)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        }
        if matches!(
            entry.snapshot.state,
            ChannelState::Down | ChannelState::Rsrved | ChannelState::Ringing
        ) {
            return Err(ControlError::InvalidState("Channel in invalid state"));
        }
        if !matches!(entry.call.state(), CallState::Up | CallState::Ringing) {
            return Err(ControlError::Unsupported);
        }
        if selection.is_err() && entry.music.is_some() {
            let mut outcome = Outcome::empty();
            outcome.effects.push(Effect::MusicClassUnavailable {
                key: CallKey {
                    id: channel,
                    generation: entry.call.generation(),
                },
            });
            return Ok(outcome);
        }
        if entry
            .music
            .as_ref()
            .is_some_and(|music| music.class() == class.as_deref())
        {
            return Ok(Outcome::empty());
        }
        if self.music_source_count() >= 64 {
            return Err(ControlError::Capacity);
        }
        let key = CallKey {
            id: channel,
            generation: entry.call.generation(),
        };
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::Capacity)?;
        let token = entry.media_tasks.reserve(MediaTaskKind::Playback)?;
        let mut outcome = Outcome::empty();
        if let Some(previous) = entry.music.take() {
            if !entry.media_tasks.finish(previous.token()) {
                entry.music = Some(previous);
                if !entry.media_tasks.finish(token) {
                    return Err(ControlError::Conflict("Music replacement rollback failed"));
                }
                return Err(ControlError::Conflict("Music ownership changed"));
            }
            outcome.effects.push(Effect::ControlPlayback {
                token: previous.token(),
                operation: PlaybackControl::Stop,
            });
        }
        entry.music = Some(MusicTask::Selected {
            token,
            class,
            selection,
        });
        self.next_playback(&MediaOwner::Channel(key), &mut outcome)?;
        Ok(outcome)
    }

    pub(super) fn stop_music(&mut self, channel: ChannelId) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get(&channel)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if !matches!(entry.call.stasis(), StasisMembership::Entered(_)) {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        }
        if matches!(
            entry.snapshot.state,
            ChannelState::Down | ChannelState::Rsrved | ChannelState::Ringing
        ) {
            return Err(ControlError::InvalidState("Channel in invalid state"));
        }
        let key = CallKey {
            id: channel,
            generation: entry.call.generation(),
        };
        let mut outcome = Outcome::empty();
        self.stop_music_for_playback(&key, &mut outcome)?;
        Ok(outcome)
    }

    pub(super) fn stop_music_for_playback(
        &mut self,
        key: &CallKey,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        let Some(entry) = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
        else {
            return Ok(());
        };
        let Some(music) = entry.music.take() else {
            return Ok(());
        };
        if !entry.media_tasks.finish(music.token()) {
            entry.music = Some(music);
            return Err(ControlError::Conflict("Music ownership changed"));
        }
        outcome.effects.push(Effect::ControlPlayback {
            token: music.token(),
            operation: PlaybackControl::Stop,
        });
        self.next_playback(&MediaOwner::Channel(key.clone()), outcome)
    }

    pub(super) fn next_music(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
        outcome: &mut Outcome,
    ) -> Result<bool, ControlError> {
        let MediaOwner::Channel(key) = owner else {
            return Ok(false);
        };
        let Some(entry) = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
        else {
            return Ok(false);
        };
        if !entry
            .music
            .as_ref()
            .is_some_and(|music| music.token() == token)
        {
            return Ok(false);
        }
        entry.media_tasks.start(token)?;
        let music = entry.music.take().ok_or(ControlError::Capacity)?;
        let MusicTask::Selected {
            token,
            class,
            selection,
        } = music
        else {
            entry.music = Some(music);
            return Err(ControlError::Conflict("Music selection already consumed"));
        };
        if entry.call.state() == CallState::Ringing && entry.call.media() == MediaState::Waiting {
            entry.music = Some(MusicTask::Preparing {
                token,
                class,
                selection,
            });
            outcome.effects.push(Effect::PrepareEarlyMusic {
                key: key.clone(),
                token,
            });
            return Ok(true);
        }
        entry.music = Some(MusicTask::Running { token, class });
        outcome.effects.push(Effect::StartMusic {
            key: key.clone(),
            token,
            selection,
        });
        Ok(true)
    }

    pub(super) fn early_media_prepared(
        &mut self,
        key: CallKey,
        token: MediaTaskToken,
        result: Result<(), ControlError>,
    ) -> Result<Outcome, ControlError> {
        let Some(entry) = self.channels.get_mut(&key.id).filter(|entry| {
            entry.call.generation() == key.generation
                && matches!(entry.music.as_ref(), Some(MusicTask::Preparing { token: current, .. }) if *current == token)
        }) else {
            return Ok(Outcome::empty());
        };
        if result.is_err() {
            return Ok(self
                .finish_music(&MediaOwner::Channel(key), token)?
                .unwrap_or_else(Outcome::empty));
        }
        let call = if entry.call.state() == CallState::Ringing {
            entry.call.admit_early_media()?
        } else {
            entry.call
        };
        let music = entry.music.take().ok_or(ControlError::Capacity)?;
        let MusicTask::Preparing {
            token,
            class,
            selection,
        } = music
        else {
            entry.music = Some(music);
            return Err(ControlError::Conflict(
                "Early music preparation ownership changed",
            ));
        };
        entry.call = call;
        entry.music = Some(MusicTask::Running { token, class });
        let mut outcome = Outcome::empty();
        outcome.effects.push(Effect::Gate {
            key: key.clone(),
            gate: call.audio_gate(),
            generated_audio_gate: call.generated_audio_gate(),
        });
        outcome.effects.push(Effect::StartMusic {
            key,
            token,
            selection,
        });
        Ok(outcome)
    }

    pub(super) fn owns_music(&self, owner: &MediaOwner, token: MediaTaskToken) -> bool {
        let MediaOwner::Channel(key) = owner else {
            return false;
        };
        self.channels.get(&key.id).is_some_and(|entry| {
            entry.call.generation() == key.generation
                && entry
                    .music
                    .as_ref()
                    .is_some_and(|music| music.token() == token)
        })
    }

    pub(super) fn finish_music(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
    ) -> Result<Option<Outcome>, ControlError> {
        if !self.owns_music(owner, token) {
            return Ok(None);
        }
        let MediaOwner::Channel(key) = owner else {
            return Ok(None);
        };
        let entry = self
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::Capacity)?;
        if !entry.media_tasks.finish(token) {
            return Err(ControlError::Conflict("Music completion changed"));
        }
        entry.music = None;
        let mut outcome = Outcome::empty();
        self.next_playback(owner, &mut outcome)?;
        Ok(Some(outcome))
    }
}
