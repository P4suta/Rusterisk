use super::{CallKey, Control, ControlError, Effect, MediaOwner, Outcome, StasisMembership};
use crate::media::PlaybackControl;
use crate::storage::{MusicSelection, PreparedMusic};
use rusterisk_api::{ChannelId, ChannelState};
use rusterisk_core::{CallState, MediaTaskKind, MediaTaskToken};

pub(super) struct MusicTask {
    pub(super) token: MediaTaskToken,
    class: Option<String>,
    selection: Option<Result<MusicSelection, ControlError>>,
}

impl Control {
    pub(crate) fn with_music(mut self, catalog: PreparedMusic) -> Self {
        self.music_catalog = Some(catalog);
        self
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
        if entry.call.state() != CallState::Up {
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
            .is_some_and(|music| music.class == class)
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
            if !entry.media_tasks.finish(previous.token) {
                entry.music = Some(previous);
                if !entry.media_tasks.finish(token) {
                    return Err(ControlError::Conflict("Music replacement rollback failed"));
                }
                return Err(ControlError::Conflict("Music ownership changed"));
            }
            outcome.effects.push(Effect::ControlPlayback {
                token: previous.token,
                operation: PlaybackControl::Stop,
            });
        }
        entry.music = Some(MusicTask {
            token,
            class,
            selection: Some(selection),
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
        if !entry.media_tasks.finish(music.token) {
            entry.music = Some(music);
            return Err(ControlError::Conflict("Music ownership changed"));
        }
        outcome.effects.push(Effect::ControlPlayback {
            token: music.token,
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
        let Some(music) = entry.music.as_mut().filter(|music| music.token == token) else {
            return Ok(false);
        };
        entry.media_tasks.start(token)?;
        let selection = music
            .selection
            .take()
            .ok_or(ControlError::Conflict("Music selection already consumed"))?;
        outcome.effects.push(Effect::StartMusic {
            key: key.clone(),
            token,
            selection,
        });
        Ok(true)
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
                    .is_some_and(|music| music.token == token)
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
