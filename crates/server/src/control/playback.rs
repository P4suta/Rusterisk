use super::{
    application_name, CallKey, Control, ControlError, Effect, MediaOwner, Outcome,
    PlaybackCompletion,
};
use crate::media::PlaybackControl;
use rusterisk_api::{ChannelId, ChannelState, EventKind, MediaUri, Playback, PlaybackState};
use rusterisk_core::{MediaTaskKind, MediaTaskToken, MediaTasks, StasisMembership};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Loading,
    Playing,
    Paused,
    Stopping,
}

pub(super) struct PlaybackEntry {
    owner: MediaOwner,
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
            target_uri: match &self.owner {
                MediaOwner::Channel(key) => format!("channel:{}", key.id),
                MediaOwner::Bridge { id, .. } => format!("bridge:{id}"),
            },
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
    pub(super) fn media_tasks(
        &mut self,
        owner: &MediaOwner,
    ) -> Result<&mut MediaTasks, ControlError> {
        match owner {
            MediaOwner::Channel(key) => self
                .channels
                .get_mut(&key.id)
                .filter(|entry| entry.call.generation() == key.generation)
                .map(|entry| &mut entry.media_tasks)
                .ok_or(ControlError::NotFound("Channel not found")),
            MediaOwner::Bridge { id, generation } => self
                .bridges
                .get_mut(id)
                .filter(|entry| entry.token == *generation)
                .map(|entry| &mut entry.media_tasks)
                .ok_or(ControlError::NotFound("Bridge not found")),
        }
    }

    fn media_event(
        &self,
        owner: &MediaOwner,
        timestamp: &str,
        kind: EventKind,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        outcome.media_notice(owner, timestamp, &kind);
        match owner {
            MediaOwner::Channel(key) => {
                let entry = self
                    .channels
                    .get(&key.id)
                    .filter(|entry| entry.call.generation() == key.generation)
                    .ok_or(ControlError::NotFound("Channel not found"))?;
                outcome.optional_event(
                    application_name(&self.applications, entry.call.stasis())?,
                    timestamp,
                    kind,
                );
            }
            MediaOwner::Bridge { .. } => {
                for application in self.applications.keys() {
                    outcome.event(application, timestamp, kind.clone());
                }
            }
        }
        Ok(())
    }

    pub(super) fn play_channel(
        &mut self,
        channel: ChannelId,
        id: ChannelId,
        media: MediaUri,
        language: Option<ChannelId>,
    ) -> Result<Outcome, ControlError> {
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
        let language = language.map_or_else(
            || entry.snapshot.language.clone(),
            |value| value.to_string(),
        );
        self.enqueue_playback(MediaOwner::Channel(key), id, media, language)
    }

    pub(super) fn play_bridge(
        &mut self,
        bridge: ChannelId,
        id: ChannelId,
        media: MediaUri,
        language: Option<ChannelId>,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .bridges
            .get(&bridge)
            .ok_or(ControlError::NotFound("Bridge not found"))?;
        let owner = MediaOwner::Bridge {
            id: bridge,
            generation: entry.token,
        };
        self.enqueue_playback(
            owner,
            id,
            media,
            language.map_or_else(String::new, |value| value.to_string()),
        )
    }

    fn enqueue_playback(
        &mut self,
        owner: MediaOwner,
        id: ChannelId,
        media: MediaUri,
        language: String,
    ) -> Result<Outcome, ControlError> {
        if self.playbacks.contains_key(&id) {
            return Err(ControlError::Conflict("Playback already exists"));
        }
        if self.playbacks.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let token = self.media_tasks(&owner)?.reserve(MediaTaskKind::Playback)?;
        let playback = PlaybackEntry {
            owner: owner.clone(),
            token,
            media,
            language,
            phase: Phase::Queued,
        };
        let mut outcome = Outcome::json(playback.snapshot(&id))?;
        outcome.status = 201;
        outcome.location = Some(format!("/ari/playbacks/{id}"));
        self.playbacks.insert(id, playback);
        self.next_playback(&owner, &mut outcome)?;
        Ok(outcome)
    }

    fn next_playback(
        &mut self,
        owner: &MediaOwner,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        if let Some(token) = self.media_tasks(owner)?.next_playback() {
            let (id, playback) = self
                .playbacks
                .iter()
                .find(|(_, playback)| playback.owner == *owner && playback.token == token)
                .ok_or(ControlError::Conflict("Playback ownership is missing"))?;
            let id = id.clone();
            let media = playback.media.clone();
            let language = playback.language.clone();
            self.media_tasks(owner)?.start(token)?;
            self.playbacks
                .get_mut(&id)
                .ok_or(ControlError::NotFound("Playback not found"))?
                .phase = Phase::Loading;
            outcome.effects.push(Effect::PlayMedia {
                owner: owner.clone(),
                token,
                media,
                language,
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
        owner: &MediaOwner,
        token: MediaTaskToken,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let mut outcome = Outcome::empty();
        if let Some((id, playback)) = self
            .playbacks
            .iter_mut()
            .find(|(_, playback)| playback.owner == *owner && playback.token == token)
        {
            if playback.phase == Phase::Loading {
                playback.phase = Phase::Playing;
                let snapshot = playback.snapshot(id);
                self.media_event(
                    owner,
                    timestamp,
                    EventKind::PlaybackStarted { playback: snapshot },
                    &mut outcome,
                )?;
            }
        }
        Ok(outcome)
    }

    pub(super) fn finish_playback(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
        completion: PlaybackCompletion,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let id = self.playbacks.iter().find_map(|(id, playback)| {
            (playback.owner == *owner && playback.token == token).then_some(id.clone())
        });
        let Some(id) = id else {
            return match owner {
                MediaOwner::Channel(key) => self.finish_dialplan_playback(key, token, completion),
                MediaOwner::Bridge { .. } => Ok(Outcome::empty()),
            };
        };
        if !self.media_tasks(owner)?.finish(token) {
            return Err(ControlError::Conflict("Playback completion changed"));
        }
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
        self.media_event(
            owner,
            timestamp,
            EventKind::PlaybackFinished { playback: snapshot },
            &mut outcome,
        )?;
        self.next_playback(owner, &mut outcome)?;
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
            .get(id)
            .ok_or(ControlError::NotFound("Playback not found"))?;
        let owner = playback.owner.clone();
        let token = playback.token;
        let phase = playback.phase;
        let mut outcome = Outcome::empty();
        let next = match (operation, phase) {
            (PlaybackControl::Restart, Phase::Queued | Phase::Loading) => return Ok(outcome),
            (
                PlaybackControl::Restart | PlaybackControl::Forward | PlaybackControl::Reverse,
                Phase::Playing,
            ) => Phase::Playing,
            (
                PlaybackControl::Restart | PlaybackControl::Forward | PlaybackControl::Reverse,
                Phase::Paused | Phase::Stopping,
            )
            | (
                PlaybackControl::Forward | PlaybackControl::Reverse,
                Phase::Queued | Phase::Loading,
            ) => return Err(ControlError::Conflict("Playback not playing")),
            (PlaybackControl::Stop, Phase::Queued) => {
                return self.finish_playback(
                    &owner,
                    token,
                    PlaybackCompletion::Completed,
                    timestamp,
                )
            }
            (PlaybackControl::Stop, Phase::Stopping) => return Ok(outcome),
            (PlaybackControl::Stop, Phase::Loading | Phase::Playing | Phase::Paused) => {
                Phase::Stopping
            }
            (PlaybackControl::Pause, Phase::Playing | Phase::Paused) => {
                self.media_tasks(&owner)?.pause(token)?;
                Phase::Paused
            }
            (PlaybackControl::Resume, Phase::Playing | Phase::Paused) => {
                self.media_tasks(&owner)?.resume(token)?;
                Phase::Playing
            }
            (
                PlaybackControl::Pause | PlaybackControl::Resume,
                Phase::Queued | Phase::Loading | Phase::Stopping,
            ) => return Err(ControlError::Conflict("Playback not playing")),
        };
        let playback = self
            .playbacks
            .get_mut(id)
            .ok_or(ControlError::NotFound("Playback not found"))?;
        playback.phase = next;
        if operation == PlaybackControl::Resume && phase == Phase::Paused {
            let snapshot = playback.snapshot(id);
            self.media_event(
                &owner,
                timestamp,
                EventKind::PlaybackStarted { playback: snapshot },
                &mut outcome,
            )?;
        }
        outcome
            .effects
            .push(Effect::ControlPlayback { token, operation });
        Ok(outcome)
    }

    fn remove_playbacks(&mut self, owner: &MediaOwner) -> Vec<Playback> {
        let ids = self
            .playbacks
            .iter()
            .filter_map(|(id, playback)| (playback.owner == *owner).then_some(id.clone()))
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(playback) = self.playbacks.remove(&id) {
                let mut snapshot = playback.snapshot(&id);
                snapshot.state = PlaybackState::Done;
                snapshots.push(snapshot);
            }
        }
        snapshots
    }

    pub(super) fn end_channel_playbacks(
        &mut self,
        key: &CallKey,
        application: Option<&str>,
        timestamp: &str,
        outcome: &mut Outcome,
    ) {
        for playback in self.remove_playbacks(&MediaOwner::Channel(key.clone())) {
            outcome.media_notice(
                &MediaOwner::Channel(key.clone()),
                timestamp,
                &EventKind::PlaybackFinished {
                    playback: playback.clone(),
                },
            );
            outcome.optional_event(
                application,
                timestamp,
                EventKind::PlaybackFinished { playback },
            );
        }
    }

    pub(super) fn end_bridge_playbacks(
        &mut self,
        id: &ChannelId,
        generation: u64,
        timestamp: &str,
        outcome: &mut Outcome,
    ) {
        let owner = MediaOwner::Bridge {
            id: id.clone(),
            generation,
        };
        for playback in self.remove_playbacks(&owner) {
            outcome.media_notice(
                &owner,
                timestamp,
                &EventKind::PlaybackFinished {
                    playback: playback.clone(),
                },
            );
            for application in self.applications.keys() {
                outcome.event(
                    application,
                    timestamp,
                    EventKind::PlaybackFinished {
                        playback: playback.clone(),
                    },
                );
            }
        }
    }
}
