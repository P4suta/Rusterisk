use super::{
    CallHistoryEvent, CallKey, ChannelEntry, ChannelKind, Control, ControlError, Effect, Outcome,
};
use rusterisk_api::{
    Caller, Channel, ChannelId, ChannelState, Dialplan, EventKind, ExternalRtpTarget,
};
use rusterisk_core::{Call, MediaTasks};
use std::{collections::BTreeMap, net::SocketAddr};

impl Control {
    pub(super) fn external_rtp(
        &mut self,
        id: ChannelId,
        app: String,
        target: ExternalRtpTarget,
        data: String,
        timestamp: &str,
        now: u64,
    ) -> Result<Outcome, ControlError> {
        if self.channels.contains_key(&id) {
            return Err(ControlError::Conflict("Channel already exists"));
        }
        if self.channels.len() >= self.capacity {
            return Err(ControlError::Capacity);
        }
        ChannelId::try_from(app.clone())?;
        if data.len() > 4096
            || data.split(',').count() > 32
            || data.split(',').any(|value| value.len() > 256)
        {
            return Err(ControlError::Invalid("Invalid Stasis arguments"));
        }
        let application = self
            .applications
            .get(&app)
            .copied()
            .ok_or(ControlError::NotFound("Application not found"))?;
        let deadline = now.checked_add(5000).ok_or(ControlError::Capacity)?;
        let generation = self.generation()?;
        let key = CallKey {
            id: id.clone(),
            generation,
        };
        let snapshot = Channel {
            id: id.clone(),
            name: format!("UnicastRTP/{}-{generation:08x}", target.remote()),
            state: ChannelState::Down,
            accountcode: String::new(),
            caller: Caller::default(),
            connected: Caller::default(),
            dialplan: Dialplan {
                context: "default".into(),
                exten: "s".into(),
                priority: 1,
                app_name: String::new(),
                app_data: String::new(),
            },
            creationtime: timestamp.into(),
            language: "en".into(),
            channelvars: None,
        };
        let mut outcome = Outcome::json(&snapshot)?;
        outcome.history.push(CallHistoryEvent::Started {
            key: key.clone(),
            timestamp: timestamp.into(),
        });
        outcome.event(
            &app,
            timestamp,
            EventKind::ChannelCreated {
                channel: snapshot.clone(),
            },
        );
        outcome
            .effects
            .push(Effect::ConnectExternalRtp { key, target });
        self.channels.insert(
            id,
            ChannelEntry {
                kind: ChannelKind::ExternalRtpPending,
                call: Call::outgoing(generation).reserve_stasis(application)?,
                endpoint: String::new(),
                snapshot,
                arguments: if data.is_empty() {
                    Vec::new()
                } else {
                    data.split(',').map(str::to_owned).collect()
                },
                deadline: Some(deadline),
                variables: BTreeMap::new(),
                dtmf: None,
                dialplan: None,
                media_tasks: MediaTasks::new(generation),
            },
        );
        Ok(outcome)
    }

    pub(super) fn external_rtp_admitted(
        &mut self,
        key: CallKey,
        result: Result<SocketAddr, ControlError>,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let Some(entry) = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
        else {
            let mut outcome = Outcome::empty();
            outcome.effects.push(Effect::Release { key });
            return Ok(outcome);
        };
        match entry.kind {
            ChannelKind::Sip | ChannelKind::ExternalRtpAdmitted => return Ok(Outcome::empty()),
            ChannelKind::ExternalRtpPending => {}
        }
        let local = match result {
            Ok(local) if local.port() != 0 && !local.ip().is_unspecified() => local,
            Ok(_) | Err(_) => return self.end(&key.id, timestamp, 21),
        };
        entry.kind = ChannelKind::ExternalRtpAdmitted;
        let variables: BTreeMap<String, String> = [
            ("UNICASTRTP_LOCAL_ADDRESS".into(), local.ip().to_string()),
            ("UNICASTRTP_LOCAL_PORT".into(), local.port().to_string()),
        ]
        .into_iter()
        .collect();
        entry.variables.extend(variables.clone());
        let mut snapshot = entry.snapshot.clone();
        snapshot.channelvars = Some(variables);
        Outcome::json(snapshot)
    }
}
