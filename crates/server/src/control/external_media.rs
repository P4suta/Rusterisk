use super::{
    CallHistoryEvent, CallKey, ChannelEntry, ChannelKind, Control, ControlError, Effect, Outcome,
};
use rusterisk_api::{
    Caller, Channel, ChannelId, ChannelState, Dialplan, EventKind, ExternalRtpTarget,
};
use rusterisk_core::{Call, MediaTasks};
use std::{collections::BTreeMap, net::SocketAddr};

pub(super) enum Target {
    Rtp(ExternalRtpTarget),
    AudioSocket(rusterisk_api::AudioSocketTarget),
    WebSocket(crate::media_websocket::MediaLinkOptions),
    WebSocketClient {
        client: rusterisk_config::WebSocketClient,
        options: rusterisk_api::WebSocketMediaOptions,
    },
}

pub(super) struct Creation {
    pub id: ChannelId,
    pub app: String,
    pub target: Target,
    pub data: String,
    pub initial: super::initial_variables::InitialVariables,
}

impl Control {
    pub(super) fn media_control_format(
        &self,
        format: Option<rusterisk_api::MediaControlFormat>,
    ) -> rusterisk_api::MediaControlFormat {
        format.unwrap_or(match self.websocket.control_format {
            rusterisk_config::WebSocketControlFormat::PlainText => {
                rusterisk_api::MediaControlFormat::PlainText
            }
            rusterisk_config::WebSocketControlFormat::Json => {
                rusterisk_api::MediaControlFormat::Json
            }
        })
    }

    pub(super) fn external_media(
        &mut self,
        creation: Creation,
        timestamp: &str,
        now: u64,
    ) -> Result<Outcome, ControlError> {
        let Creation {
            id,
            app,
            target,
            data,
            initial,
        } = creation;
        if let Target::WebSocket(options) = &target {
            if self.media_connections.contains_key(&options.connection_id) {
                return Err(ControlError::Conflict("Media connection already exists"));
            }
        }
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
        let timeout = match &target {
            Target::WebSocket(_) => 30000,
            Target::Rtp(_) | Target::AudioSocket(_) | Target::WebSocketClient { .. } => 5000,
        };
        let deadline = now.checked_add(timeout).ok_or(ControlError::Capacity)?;
        let generation = self.generation()?;
        let key = CallKey {
            id: id.clone(),
            generation,
        };
        let mut variables = initial.values;
        let mut websocket = None;
        let (kind, name, effect) = match target {
            Target::WebSocketClient { client, options } => {
                let name = format!("WebSocket/{}/{generation:08x}", client.name);
                variables.insert("MEDIA_WEBSOCKET_CONNECTION_ID".into(), client.name.clone());
                variables.insert(
                    "MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE".into(),
                    options.format.optimal_frame_size().to_string(),
                );
                (
                    ChannelKind::WebSocketClientPending,
                    name,
                    Effect::ConnectWebSocket {
                        key: key.clone(),
                        client,
                        options,
                        channel_variables: variables.clone(),
                    },
                )
            }
            Target::WebSocket(mut options) => {
                let name = format!("WebSocket/INCOMING/{generation:08x}");
                options.channel = name.clone();
                variables.insert(
                    "MEDIA_WEBSOCKET_CONNECTION_ID".into(),
                    options.connection_id.to_string(),
                );
                variables.insert(
                    "MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE".into(),
                    options.format.optimal_frame_size().to_string(),
                );
                options.channel_variables = variables.clone();
                websocket = Some(options);
                (
                    ChannelKind::WebSocketPending,
                    name,
                    Effect::ReserveWebSocket { key: key.clone() },
                )
            }
            Target::Rtp(target) => (
                ChannelKind::ExternalRtpPending,
                format!("UnicastRTP/{}-{generation:08x}", target.remote()),
                Effect::ConnectExternalRtp {
                    key: key.clone(),
                    target,
                },
            ),
            Target::AudioSocket(target) => (
                ChannelKind::AudioSocketPending,
                format!("AudioSocket/{}-{generation:08x}", target.remote()),
                Effect::ConnectAudioSocket {
                    key: key.clone(),
                    target,
                },
            ),
        };
        let snapshot = Channel {
            id: id.clone(),
            name,
            state: ChannelState::Down,
            accountcode: String::new(),
            caller: initial.caller,
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
            channelvars: (!variables.is_empty()).then(|| variables.clone()),
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
        outcome.effects.push(effect);
        if let Some(options) = websocket {
            self.media_connections
                .insert(options.connection_id.clone(), (key.clone(), options));
        }
        self.channels.insert(
            id,
            ChannelEntry {
                kind,
                call: Call::outgoing(generation).reserve_stasis(application)?,
                endpoint: String::new(),
                snapshot,
                arguments: if data.is_empty() {
                    Vec::new()
                } else {
                    data.split(',').map(str::to_owned).collect()
                },
                deadline: Some(deadline),
                variables,
                dtmf: None,
                dialplan: None,
                media_tasks: MediaTasks::new(generation),
                music: None,
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
            ChannelKind::Sip
            | ChannelKind::ExternalRtpAdmitted
            | ChannelKind::AudioSocketPending
            | ChannelKind::AudioSocketAdmitted
            | ChannelKind::WebSocketPending
            | ChannelKind::WebSocketClaimed => return Ok(Outcome::empty()),
            ChannelKind::WebSocketClientPending | ChannelKind::WebSocketClientAdmitted => {
                return Ok(Outcome::empty())
            }
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
        entry.variables.extend(variables);
        let mut snapshot = entry.snapshot.clone();
        snapshot.channelvars = Some(entry.variables.clone());
        Outcome::json(snapshot)
    }

    pub(super) fn audio_socket_admitted(
        &mut self,
        key: CallKey,
        result: Result<(), ControlError>,
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
            ChannelKind::AudioSocketPending => {}
            ChannelKind::Sip
            | ChannelKind::ExternalRtpPending
            | ChannelKind::ExternalRtpAdmitted
            | ChannelKind::AudioSocketAdmitted
            | ChannelKind::WebSocketPending
            | ChannelKind::WebSocketClaimed => return Ok(Outcome::empty()),
            ChannelKind::WebSocketClientPending | ChannelKind::WebSocketClientAdmitted => {
                return Ok(Outcome::empty())
            }
        }
        match result {
            Ok(()) => {}
            Err(_) => return self.end(&key.id, timestamp, 21),
        }
        entry.kind = ChannelKind::AudioSocketAdmitted;
        Outcome::json(&entry.snapshot)
    }

    pub(super) fn websocket_client_admitted(
        &mut self,
        key: CallKey,
        result: Result<(), ControlError>,
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
            ChannelKind::WebSocketClientPending => {}
            ChannelKind::Sip
            | ChannelKind::ExternalRtpPending
            | ChannelKind::ExternalRtpAdmitted
            | ChannelKind::AudioSocketPending
            | ChannelKind::AudioSocketAdmitted
            | ChannelKind::WebSocketPending
            | ChannelKind::WebSocketClaimed
            | ChannelKind::WebSocketClientAdmitted => return Ok(Outcome::empty()),
        }
        match result {
            Ok(()) => {}
            Err(_) => return self.end(&key.id, timestamp, 21),
        }
        entry.kind = ChannelKind::WebSocketClientAdmitted;
        Outcome::json(&entry.snapshot)
    }

    pub fn claim_media_connection(
        &mut self,
        connection_id: &ChannelId,
    ) -> Result<(CallKey, crate::media_websocket::MediaLinkOptions), ControlError> {
        let (key, options) = self
            .media_connections
            .get(connection_id)
            .ok_or(ControlError::NotFound("WebSocket instance not found"))?;
        let entry = self
            .channels
            .get_mut(&key.id)
            .filter(|entry| entry.call.generation() == key.generation)
            .ok_or(ControlError::NotFound("WebSocket instance not found"))?;
        match entry.call.connection_claim() {
            rusterisk_core::ConnectionClaim::Available => {}
            rusterisk_core::ConnectionClaim::Claimed => {
                return Err(ControlError::Conflict(
                    "Another websocket connection exists for this connection id",
                ))
            }
            rusterisk_core::ConnectionClaim::Released => {
                return Err(ControlError::NotFound("WebSocket instance not found"))
            }
        }
        entry.call = entry.call.claim_connection()?;
        entry.kind = ChannelKind::WebSocketClaimed;
        Ok((key.clone(), options.clone()))
    }
}
