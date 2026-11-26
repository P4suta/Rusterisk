use super::{application_name, CallKey, Control, ControlError, Effect, MediaOwner, Outcome};
use crate::media::RecordingEnd;
use rusterisk_api::{ChannelId, EventKind, LiveRecording, RecordingOptions, RecordingState};
use rusterisk_core::{
    MediaTaskKind, MediaTaskToken, RecordingControl, RecordingGate, StasisApplication,
    StasisMembership,
};

#[derive(Clone, Copy)]
enum Phase {
    Pending,
    Active,
    Ending(RecordingEnd),
}

pub(super) struct RecordingEntry {
    owner: MediaOwner,
    token: MediaTaskToken,
    name: ChannelId,
    phase: Phase,
    gate: RecordingGate,
    application: Option<StasisApplication>,
}

impl RecordingEntry {
    fn snapshot(&self) -> LiveRecording {
        LiveRecording {
            name: self.name.to_string(),
            format: "wav".into(),
            state: match self.phase {
                Phase::Pending => RecordingState::Queued,
                Phase::Active => match self.gate {
                    RecordingGate::Open | RecordingGate::Silent => RecordingState::Recording,
                    RecordingGate::Paused | RecordingGate::PausedSilent => RecordingState::Paused,
                },
                Phase::Ending(RecordingEnd::Finish) => RecordingState::Done,
                Phase::Ending(RecordingEnd::Cancel) => RecordingState::Canceled,
            },
            target_uri: match &self.owner {
                MediaOwner::Channel(key) => format!("channel:{}", key.id),
                MediaOwner::Bridge { id, .. } => format!("bridge:{id}"),
            },
            duration: None,
            talking_duration: None,
            silence_duration: None,
            cause: None,
        }
    }
}

impl Control {
    pub(crate) fn cancel_recording_admission(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
    ) -> Result<Outcome, ControlError> {
        let Some(entry) = self
            .recordings
            .values()
            .find(|entry| entry.owner == *owner && entry.token == token)
        else {
            return Ok(Outcome::empty());
        };
        if matches!(entry.phase, Phase::Ending(_)) {
            return Ok(Outcome::empty());
        }
        let name = entry.name.clone();
        self.end_recording(&name, RecordingEnd::Cancel)
    }

    fn recording_event(
        &self,
        entry: &RecordingEntry,
        timestamp: &str,
        kind: EventKind,
        outcome: &mut Outcome,
    ) -> Result<(), ControlError> {
        outcome.media_notice(&entry.owner, timestamp, &kind);
        match &entry.owner {
            MediaOwner::Channel(key) => {
                let live = self
                    .channels
                    .get(&key.id)
                    .filter(|channel| channel.call.generation() == key.generation);
                let application = if let Some(channel) = live {
                    application_name(&self.applications, channel.call.stasis())?
                } else {
                    entry.application.and_then(|expected| {
                        self.applications.iter().find_map(|(name, current)| {
                            (*current == expected).then_some(name.as_str())
                        })
                    })
                };
                outcome.optional_event(application, timestamp, kind);
            }
            MediaOwner::Bridge { .. } => {
                for application in self.applications.keys() {
                    outcome.event(application, timestamp, kind.clone());
                }
            }
        }
        Ok(())
    }

    pub(super) fn record_channel(
        &mut self,
        channel: ChannelId,
        name: ChannelId,
        options: RecordingOptions,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .channels
            .get(&channel)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let StasisMembership::Entered(application) = entry.call.stasis() else {
            return Err(ControlError::Conflict("Channel not in Stasis application"));
        };
        let owner = MediaOwner::Channel(CallKey {
            id: channel,
            generation: entry.call.generation(),
        });
        self.enqueue_recording(owner, name, options, Some(application))
    }

    pub(super) fn record_bridge(
        &mut self,
        bridge: ChannelId,
        name: ChannelId,
        options: RecordingOptions,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .bridges
            .get(&bridge)
            .ok_or(ControlError::NotFound("Bridge not found"))?;
        self.enqueue_recording(
            MediaOwner::Bridge {
                id: bridge,
                generation: entry.token,
            },
            name,
            options,
            None,
        )
    }

    fn enqueue_recording(
        &mut self,
        owner: MediaOwner,
        name: ChannelId,
        options: RecordingOptions,
        application: Option<StasisApplication>,
    ) -> Result<Outcome, ControlError> {
        if self.recordings.contains_key(&name) {
            return Err(ControlError::Conflict("Recording already exists"));
        }
        if self.recordings.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let tasks = self.media_tasks(&owner)?;
        let token = tasks.reserve(MediaTaskKind::Recording)?;
        tasks.start(token)?;
        let entry = RecordingEntry {
            owner: owner.clone(),
            token,
            name: name.clone(),
            phase: Phase::Pending,
            gate: RecordingGate::Open,
            application,
        };
        let mut outcome = Outcome::json(entry.snapshot())?;
        outcome.status = 201;
        outcome.location = Some(format!("/ari/recordings/live/{name}"));
        outcome.effects.push(Effect::RecordMedia {
            owner,
            token,
            name: name.clone(),
            options,
        });
        self.recordings.insert(name, entry);
        Ok(outcome)
    }

    pub(super) fn get_recording(&self, name: &ChannelId) -> Result<Outcome, ControlError> {
        Outcome::json(
            self.recordings
                .get(name)
                .ok_or(ControlError::NotFound("Recording not found"))?
                .snapshot(),
        )
    }

    pub(super) fn recording_admitted(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
        result: Result<(), ControlError>,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        if let Err(error) = result {
            return self.finish_recording(owner, token, Err(error), timestamp);
        }
        let mut outcome = Outcome::empty();
        if let Some(entry) = self
            .recordings
            .values_mut()
            .find(|entry| entry.owner == *owner && entry.token == token)
        {
            if matches!(entry.phase, Phase::Pending) {
                entry.phase = Phase::Active;
                let name = entry.name.clone();
                let snapshot = entry.snapshot();
                self.recording_event(
                    self.recordings.get(&name).ok_or(ControlError::Capacity)?,
                    timestamp,
                    EventKind::RecordingStarted {
                        recording: snapshot,
                    },
                    &mut outcome,
                )?;
            }
        }
        Ok(outcome)
    }

    pub(super) fn control_recording(
        &mut self,
        name: &ChannelId,
        operation: RecordingControl,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .recordings
            .get(name)
            .ok_or(ControlError::NotFound("Recording not found"))?;
        if !matches!(entry.phase, Phase::Active) {
            return Err(ControlError::Conflict("Recording not in session"));
        }
        let owner = entry.owner.clone();
        let token = entry.token;
        match operation {
            RecordingControl::Pause => self.media_tasks(&owner)?.pause(token)?,
            RecordingControl::Resume => self.media_tasks(&owner)?.resume(token)?,
            RecordingControl::Mute | RecordingControl::Unmute => {}
        }
        let entry = self
            .recordings
            .get_mut(name)
            .ok_or(ControlError::NotFound("Recording not found"))?;
        entry.gate = entry.gate.apply(operation);
        let mut outcome = Outcome::empty();
        outcome
            .effects
            .push(Effect::ControlRecording { token, operation });
        Ok(outcome)
    }

    pub(super) fn end_recording(
        &mut self,
        name: &ChannelId,
        mode: RecordingEnd,
    ) -> Result<Outcome, ControlError> {
        let entry = self
            .recordings
            .get_mut(name)
            .ok_or(ControlError::NotFound("Recording not found"))?;
        if matches!(entry.phase, Phase::Ending(_)) {
            return Err(ControlError::Conflict("Recording not in session"));
        }
        entry.phase = Phase::Ending(mode);
        let mut outcome = Outcome::empty();
        outcome.effects.push(Effect::EndRecording {
            owner: entry.owner.clone(),
            token: entry.token,
            mode,
        });
        Ok(outcome)
    }

    pub(super) fn finish_owner_recordings(&mut self, owner: &MediaOwner) {
        for entry in self
            .recordings
            .values_mut()
            .filter(|entry| entry.owner == *owner)
        {
            if !matches!(entry.phase, Phase::Ending(_)) {
                entry.phase = Phase::Ending(RecordingEnd::Finish);
            }
        }
    }

    pub(super) fn finish_recording(
        &mut self,
        owner: &MediaOwner,
        token: MediaTaskToken,
        result: Result<(), ControlError>,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let Some(name) = self.recordings.values().find_map(|entry| {
            (entry.owner == *owner && entry.token == token).then_some(entry.name.clone())
        }) else {
            return Ok(Outcome::empty());
        };
        match self.media_tasks(owner) {
            Ok(tasks) => {
                if !tasks.finish(token) {
                    return Err(ControlError::Conflict("Recording completion changed"));
                }
            }
            Err(ControlError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
        let entry = self
            .recordings
            .remove(&name)
            .ok_or(ControlError::NotFound("Recording not found"))?;
        let mut snapshot = entry.snapshot();
        let kind = match result {
            Ok(()) => {
                snapshot.state = if matches!(entry.phase, Phase::Ending(RecordingEnd::Cancel)) {
                    RecordingState::Canceled
                } else {
                    RecordingState::Done
                };
                EventKind::RecordingFinished {
                    recording: snapshot,
                }
            }
            Err(error) => {
                snapshot.state = RecordingState::Failed;
                snapshot.cause = Some(error.to_string());
                EventKind::RecordingFailed {
                    recording: snapshot,
                }
            }
        };
        let mut outcome = Outcome::empty();
        self.recording_event(&entry, timestamp, kind, &mut outcome)?;
        Ok(outcome)
    }
}
