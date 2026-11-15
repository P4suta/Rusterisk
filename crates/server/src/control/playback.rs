use super::{
    application_name, CallKey, Control, ControlError, Effect, Outcome, PlaybackCompletion,
};
use crate::media::PlaybackControl;
use rusterisk_api::{ChannelId, ChannelState, EventKind, MediaUri, Playback, PlaybackState};
use rusterisk_core::{MediaTaskKind, MediaTaskToken, StasisMembership};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Loading,
    Playing,
    Paused,
    Stopping,
}

pub(super) struct PlaybackEntry {
    key: CallKey,
    token: MediaTaskToken,
    media: MediaUri,
    language: String,
    phase: Phase,
}

impl PlaybackEntry {
    fn snapshot(&self, id: &ChannelId) -> Playback {
        Playback {
            id: id.clone(),
            media_uri: self.media.to_string(),
            next_media_uri: None,
            target_uri: format!("channel:{}", self.key.id),
            language: self.language.clone(),
            state: match self.phase {
                Phase::Queued | Phase::Loading => PlaybackState::Queued,
                Phase::Playing => PlaybackState::Playing,
                Phase::Paused => PlaybackState::Paused,
                Phase::Stopping => PlaybackState::Done,
            },
        }
    }
}

impl Control {
    pub(super) fn play_channel(
        &mut self,
        channel: ChannelId,
        id: ChannelId,
        media: MediaUri,
    ) -> Result<Outcome, ControlError> {
        if self.playbacks.contains_key(&id) {
            return Err(ControlError::Conflict("Playback already exists"));
        }
        if self.playbacks.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let entry = self
            .channels
            .get_mut(&channel)
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
        let token = entry.media_tasks.reserve(MediaTaskKind::Playback)?;
        let playback = PlaybackEntry {
            key: key.clone(),
            token,
            media,
            language: entry.snapshot.language.clone(),
            phase: Phase::Queued,
        };
        let mut outcome = Outcome::json(playback.snapshot(&id))?;
        outcome.status = 201;
        self.playbacks.insert(id, playback);
        self.next_playback(&key, &mut outcome)?;
        Ok(outcome)
    }

    fn next_playback(&mut self, key: &CallKey, outcome: &mut Outcome) -> Result<(), ControlError> {
        let entry = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if let Some(token) = entry.media_tasks.next_playback() {
            let playback = self
                .playbacks
                .values_mut()
                .find(|playback| playback.key == *key && playback.token == token)
                .ok_or(ControlError::Conflict("Playback ownership is missing"))?;
            entry.media_tasks.start(token)?;
            playback.phase = Phase::Loading;
            outcome.effects.push(Effect::PlayChannel {
                key: key.clone(),
                token,
                media: playback.media.clone(),
            });
        }
        Ok(())
    }

    pub(super) fn get_playback(&self, id: &ChannelId) -> Result<Outcome, ControlError> {
        let playback = self
            .playbacks
            .get(id)
            .ok_or(ControlError::NotFound("Playback not found"))?;
        Outcome::json(playback.snapshot(id))
    }

    pub(super) fn playback_started(
        &mut self,
        key: &CallKey,
        token: MediaTaskToken,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let mut outcome = Outcome::empty();
        if let Some((id, playback)) = self
            .playbacks
            .iter_mut()
            .find(|(_, playback)| playback.key == *key && playback.token == token)
        {
            if playback.phase == Phase::Loading {
                let entry = self
                    .channels
                    .get(&key.id)
                    .filter(|entry| entry.call.generation() == key.generation)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                playback.phase = Phase::Playing;
                outcome.optional_event(
                    application_name(&self.applications, entry.call.stasis())?,
                    timestamp,
                    EventKind::PlaybackStarted {
                        playback: playback.snapshot(id),
                    },
                );
            }
        }
        Ok(outcome)
    }

    pub(super) fn finish_playback(
        &mut self,
        key: &CallKey,
        token: MediaTaskToken,
        completion: PlaybackCompletion,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let id = self.playbacks.iter().find_map(|(id, playback)| {
            (playback.key == *key && playback.token == token).then_some(id.clone())
        });
        let Some(id) = id else {
            return self.finish_dialplan_playback(key, token, completion);
        };
        let entry = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        if !entry.media_tasks.finish(token) {
            return Err(ControlError::Conflict("Playback completion changed"));
        }
        let application =
            application_name(&self.applications, entry.call.stasis())?.map(str::to_owned);
        let playback = self
            .playbacks
            .remove(&id)
            .ok_or(ControlError::NotFound("Playback not found"))?;
        let mut snapshot = playback.snapshot(&id);
        snapshot.state = match (playback.phase, completion) {
            (Phase::Stopping, PlaybackCompletion::Completed | PlaybackCompletion::Failed)
            | (
                Phase::Queued | Phase::Loading | Phase::Playing | Phase::Paused,
                PlaybackCompletion::Completed,
            ) => PlaybackState::Done,
            (
                Phase::Queued | Phase::Loading | Phase::Playing | Phase::Paused,
                PlaybackCompletion::Failed,
            ) => PlaybackState::Failed,
        };
        let mut outcome = Outcome::empty();
        outcome.optional_event(
            application.as_deref(),
            timestamp,
            EventKind::PlaybackFinished { playback: snapshot },
        );
        self.next_playback(key, &mut outcome)?;
        Ok(outcome)
    }

    pub(super) fn control_playback(
        &mut self,
        id: &ChannelId,
        operation: PlaybackControl,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let playback = self
            .playbacks
            .get_mut(id)
            .ok_or(ControlError::NotFound("Playback not found"))?;
        let key = playback.key.clone();
        let token = playback.token;
        let mut outcome = Outcome::empty();
        match operation {
            PlaybackControl::Stop => match playback.phase {
                Phase::Queued => {
                    return self.finish_playback(
                        &key,
                        token,
                        PlaybackCompletion::Completed,
                        timestamp,
                    )
                }
                Phase::Stopping => return Ok(outcome),
                Phase::Loading | Phase::Playing | Phase::Paused => playback.phase = Phase::Stopping,
            },
            PlaybackControl::Pause | PlaybackControl::Resume => {
                let entry = self
                    .channels
                    .get_mut(&key.id)
                    .filter(|entry| entry.call.generation() == key.generation)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                match (operation, playback.phase) {
                    (PlaybackControl::Pause, Phase::Playing | Phase::Paused) => {
                        entry.media_tasks.pause(token)?;
                        playback.phase = Phase::Paused;
                    }
                    (PlaybackControl::Resume, Phase::Playing | Phase::Paused) => {
                        let was_paused = playback.phase == Phase::Paused;
                        entry.media_tasks.resume(token)?;
                        playback.phase = Phase::Playing;
                        if was_paused {
                            outcome.optional_event(
                                application_name(&self.applications, entry.call.stasis())?,
                                timestamp,
                                EventKind::PlaybackStarted {
                                    playback: playback.snapshot(id),
                                },
                            );
                        }
                    }
                    (
                        PlaybackControl::Pause | PlaybackControl::Resume,
                        Phase::Queued | Phase::Loading | Phase::Stopping,
                    ) => return Err(ControlError::Conflict("Playback not playing")),
                    (
                        PlaybackControl::Stop,
                        Phase::Queued
                        | Phase::Loading
                        | Phase::Playing
                        | Phase::Paused
                        | Phase::Stopping,
                    ) => return Err(ControlError::Conflict("Playback control changed")),
                }
            }
        }
        outcome
            .effects
            .push(Effect::ControlPlayback { token, operation });
        Ok(outcome)
    }

    pub(super) fn end_channel_playbacks(
        &mut self,
        key: &CallKey,
        application: Option<&str>,
        timestamp: &str,
        outcome: &mut Outcome,
    ) {
        let ids = self
            .playbacks
            .iter()
            .filter_map(|(id, playback)| (playback.key == *key).then_some(id.clone()))
            .collect::<Vec<_>>();
        for id in ids {
            if let Some(playback) = self.playbacks.remove(&id) {
                let mut snapshot = playback.snapshot(&id);
                snapshot.state = PlaybackState::Done;
                outcome.optional_event(
                    application,
                    timestamp,
                    EventKind::PlaybackFinished { playback: snapshot },
                );
            }
        }
    }
}
