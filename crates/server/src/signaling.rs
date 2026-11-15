use crate::media::{MediaNotice, MediaPort};
use crate::rtp::SocketPair;
use crate::storage::{PlaybackMedia, StoragePort};
use crate::{
    CallKey, ControlError, Controller, Effect, IncomingCall, MediaOwner, Milestone, Signal,
};
use rusterisk_api::{ChannelId, MediaUri};
use rusterisk_config::{Config, Dtmf};
use rusterisk_core::{AudioGate, DtmfCommand, MediaTaskToken, PeerMediaDirection};
use rusterisk_media::{AudioTarget, Gate};
use rusterisk_sip::{Datagram, Engine, Outbound, Output, RejectReason, SipEvent};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
mod external_media;
mod media;
use media::{PendingMedia, PendingWebRtc};
#[cfg(test)]
mod negotiation_tests;
mod outbound;
mod recording;
mod resolver;
pub(crate) mod sip_websocket;
#[cfg(test)]
mod stream_tests;
mod transport;
pub use sip_websocket::SipWebSocketPort;
#[cfg(test)]
mod sip_websocket_tests;
mod websocket;

struct Listener {
    name: String,
    engine: Engine,
}
struct Active {
    listener: usize,
    pending: Option<PendingMedia>,
    digits: DigitDelivery,
    direction_reply: Option<DirectionReply>,
}

struct DirectionReply {
    direction: PeerMediaDirection,
    transmission: transport::DeferredTransmission,
}

enum DigitDelivery {
    Media,
    Signaling(VecDeque<DtmfCommand>),
}

impl DigitDelivery {
    fn new(mode: Dtmf) -> Self {
        match mode {
            Dtmf::Rfc4733 => Self::Media,
            Dtmf::Info => Self::Signaling(VecDeque::with_capacity(8)),
        }
    }

    fn queue_mut(&mut self) -> Option<&mut VecDeque<DtmfCommand>> {
        match self {
            Self::Media => None,
            Self::Signaling(queue) => Some(queue),
        }
    }
}
struct Received {
    listener: usize,
    peer: SocketAddr,
    bytes: Vec<u8>,
}

pub struct Signaling {
    config: Config,
    controller: Controller,
    effects: mpsc::Receiver<Effect>,
    media: MediaPort,
    notices: mpsc::Receiver<MediaNotice>,
    listeners: Vec<Listener>,
    network: transport::Network,
    native_pending: BTreeMap<CallKey, outbound::Pending>,
    native_jobs: JoinSet<outbound::Completion>,
    resolver: Result<resolver::DestinationResolver, ControlError>,
    resolution_cache: resolver::ResolutionCache,
    calls: BTreeMap<CallKey, Active>,
    external_media: BTreeSet<CallKey>,
    stream_admissions: BTreeMap<CallKey, external_media::PendingStream>,
    stream_jobs: JoinSet<external_media::StreamCompletion>,
    websocket_admissions: BTreeMap<CallKey, websocket::PendingWebSocket>,
    websocket_jobs: JoinSet<websocket::Completion>,
    websocket_clients: websocket::PreparedClients,
    storage: Option<StoragePort>,
    playbacks: JoinSet<(MediaOwner, MediaTaskToken, Result<(), ControlError>)>,
    recordings: JoinSet<recording::Completion>,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn key(id: String, generation: u64) -> Result<CallKey, ControlError> {
    Ok(CallKey {
        id: ChannelId::try_from(id)?,
        generation,
    })
}

fn peer_direction(direction: rusterisk_sip::Direction) -> PeerMediaDirection {
    match direction {
        rusterisk_sip::Direction::SendRecv => PeerMediaDirection::SendReceive,
        rusterisk_sip::Direction::SendOnly => PeerMediaDirection::SendOnly,
        rusterisk_sip::Direction::RecvOnly => PeerMediaDirection::ReceiveOnly,
        rusterisk_sip::Direction::Inactive => PeerMediaDirection::Inactive,
    }
}

impl Signaling {
    pub fn websocket_port(&self) -> SipWebSocketPort {
        self.network.websocket_port(self.config.origins.clone())
    }

    pub fn with_storage(mut self, storage: StoragePort) -> Self {
        self.storage = Some(storage);
        self
    }

    pub async fn bind(
        mut config: Config,
        controller: Controller,
        effects: mpsc::Receiver<Effect>,
        media: MediaPort,
        notices: mpsc::Receiver<MediaNotice>,
    ) -> Result<Self, ControlError> {
        if config.transports.len() > rusterisk_config::MAX_TRANSPORTS {
            return Err(ControlError::Capacity);
        }
        let websocket_clients = websocket::PreparedClients::new(&config.websocket.clients)?;
        let network = transport::Network::bind(&mut config).await?;
        let mut listeners = Vec::with_capacity(config.transports.len());
        for transport in &config.transports {
            let mut scoped = config.clone();
            scoped
                .endpoints
                .retain(|endpoint| endpoint.transport == transport.name);
            let mut seed = [0; 32];
            getrandom::fill(&mut seed).map_err(failure)?;
            listeners.push(Listener {
                name: transport.name.clone(),
                engine: Engine::new(scoped, seed).with_transport(transport.protocol),
            });
        }
        Ok(Self {
            config,
            controller,
            effects,
            media,
            notices,
            listeners,
            network,
            native_pending: BTreeMap::new(),
            native_jobs: JoinSet::new(),
            resolver: resolver::DestinationResolver::system(),
            resolution_cache: resolver::ResolutionCache::default(),
            calls: BTreeMap::new(),
            external_media: BTreeSet::new(),
            stream_admissions: BTreeMap::new(),
            stream_jobs: JoinSet::new(),
            websocket_admissions: BTreeMap::new(),
            websocket_jobs: JoinSet::new(),
            websocket_clients,
            storage: None,
            playbacks: JoinSet::new(),
            recordings: JoinSet::new(),
        })
    }

    pub fn listeners(&self) -> Result<Vec<(String, SocketAddr)>, ControlError> {
        self.listeners
            .iter()
            .enumerate()
            .map(|(index, listener)| Ok((listener.name.clone(), self.network.local_addr(index)?)))
            .collect()
    }

    async fn transmit(
        &mut self,
        listener: usize,
        datagrams: Vec<Datagram>,
    ) -> Result<(), ControlError> {
        for packet in datagrams {
            self.network.send(listener, packet).await?;
        }
        Ok(())
    }

    async fn notify(&self, signal: Signal) -> Result<(), ControlError> {
        if !self.controller.is_stopping() {
            self.controller.signal(signal).await?;
        }
        Ok(())
    }

    async fn output(
        &mut self,
        listener: usize,
        output: Output,
        now: u64,
    ) -> Result<(), ControlError> {
        let mut datagrams = output.datagrams;
        for event in &output.events {
            if let SipEvent::PeerDirection {
                id,
                generation,
                direction,
            } = event
            {
                let key = key(id.clone(), *generation)?;
                if datagrams.len() != 1 {
                    return Err(ControlError::Invalid("Expected one direction answer"));
                }
                let datagram = datagrams.pop().ok_or(ControlError::Capacity)?;
                if let Some(call) = self.calls.get_mut(&key) {
                    if call.direction_reply.is_some() {
                        return Err(ControlError::Conflict("Media negotiation already pending"));
                    }
                    call.direction_reply = Some(DirectionReply {
                        direction: peer_direction(*direction),
                        transmission: self.network.defer(listener, datagram)?,
                    });
                }
            }
        }
        datagrams.retain(|packet| {
            !self.calls.values().any(|call| {
                call.direction_reply
                    .as_ref()
                    .is_some_and(|reply| reply.transmission.matches(listener, packet))
            })
        });
        self.transmit(listener, datagrams).await?;
        for event in output.events {
            match event {
                SipEvent::EndpointContact {
                    endpoint,
                    generation,
                    presence,
                } => {
                    self.notify(Signal::EndpointContact {
                        key: crate::EndpointKey {
                            resource: endpoint,
                            generation,
                        },
                        presence,
                    })
                    .await?;
                }
                SipEvent::PeerDirection {
                    id,
                    generation,
                    direction,
                } => {
                    let direction = peer_direction(direction);
                    self.notify(Signal::PeerDirection {
                        key: key(id, generation)?,
                        direction,
                    })
                    .await?;
                }
                SipEvent::DtmfReceived {
                    id,
                    generation,
                    digit,
                    duration_ms,
                } => {
                    let key = key(id, generation)?;
                    self.media
                        .received_digit(key.clone(), digit)
                        .await
                        .map_err(failure)?;
                    self.notify(Signal::DtmfReceived {
                        key,
                        digit,
                        duration_ms,
                    })
                    .await?;
                }
                SipEvent::Ringing { id, generation } => {
                    self.notify(Signal::Milestone {
                        key: key(id, generation)?,
                        milestone: Milestone::Ringing,
                    })
                    .await?
                }
                SipEvent::Answered {
                    id,
                    generation,
                    session,
                } => {
                    let key = key(id, generation)?;
                    let pending = self
                        .calls
                        .get_mut(&key)
                        .and_then(|call| call.pending.take());
                    if let Some(pending) = pending {
                        let direction = peer_direction(session.direction);
                        match pending.attach(&self.media, key.clone(), &session).await {
                            Ok(()) => {
                                self.notify(Signal::PeerDirection {
                                    key: key.clone(),
                                    direction,
                                })
                                .await?;
                                let events = match &self
                                    .calls
                                    .get(&key)
                                    .ok_or(ControlError::Capacity)?
                                    .digits
                                {
                                    DigitDelivery::Signaling(_) => Some(u16::MAX),
                                    DigitDelivery::Media => {
                                        session.telephone.map(|format| format.events())
                                    }
                                };
                                if let Some(events) = events {
                                    self.notify(Signal::DtmfReady {
                                        key: key.clone(),
                                        events,
                                    })
                                    .await?;
                                }
                                self.notify(Signal::Milestone {
                                    key,
                                    milestone: Milestone::Answered,
                                })
                                .await?
                            }
                            Err(error) => {
                                tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "media admission failed");
                                self.notify(Signal::Terminated { key, cause: 21 }).await?;
                            }
                        }
                    } else {
                        let output = self.listeners[listener]
                            .engine
                            .cancel(key.id.as_str(), key.generation, now)
                            .map_err(failure)?;
                        self.transmit(listener, output.datagrams).await?;
                    }
                }
                SipEvent::Ended {
                    id,
                    generation,
                    cause,
                } => {
                    self.notify(Signal::Terminated {
                        key: key(id, generation)?,
                        cause,
                    })
                    .await?
                }
                SipEvent::Incoming {
                    id,
                    endpoint,
                    caller,
                    context,
                    extension,
                    ..
                } => {
                    let incoming = IncomingCall {
                        id: ChannelId::try_from(id.clone())?,
                        endpoint,
                        caller,
                        context,
                        extension,
                        target: crate::IncomingTarget::Dialplan,
                    };
                    if let Err(error) = self.admit(listener, incoming).await {
                        let reason = match error {
                            ControlError::Capacity => RejectReason::Busy,
                            ControlError::NotFound(_) | ControlError::EndpointTechnology(_) => {
                                RejectReason::NoRoute
                            }
                            ControlError::Unsupported => RejectReason::Unsupported,
                            ControlError::Invalid(_)
                            | ControlError::Unprocessable(_)
                            | ControlError::InvalidState(_)
                            | ControlError::Conflict(_)
                            | ControlError::BridgeExists(_)
                            | ControlError::Unauthorized
                            | ControlError::Forbidden
                            | ControlError::Encoding(_)
                            | ControlError::Adapter(_) => RejectReason::Rejected,
                        };
                        let rejected = self.listeners[listener]
                            .engine
                            .reject(&id, reason, now)
                            .map_err(failure)?;
                        self.transmit(listener, rejected.datagrams).await?;
                    }
                }
                SipEvent::IncomingEnded { id, cause } => {
                    let key = self.calls.keys().find(|key| key.id.as_str() == id).cloned();
                    if let Some(key) = key {
                        self.notify(Signal::Terminated { key, cause }).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn admit(&mut self, listener: usize, incoming: IncomingCall) -> Result<(), ControlError> {
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
            >= 64
        {
            return Err(ControlError::Capacity);
        }
        let dtmf = self
            .config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == incoming.endpoint)
            .ok_or(ControlError::NotFound("Endpoint not found"))?
            .dtmf;
        let key = self.controller.incoming(incoming).await?;
        self.calls.insert(
            key,
            Active {
                listener,
                pending: None,
                digits: DigitDelivery::new(dtmf),
                direction_reply: None,
            },
        );
        Ok(())
    }

    async fn start(
        &mut self,
        key: CallKey,
        endpoint: String,
        caller: String,
        timeout: Option<u32>,
        now: u64,
    ) -> Result<(), ControlError> {
        self.queue_native(key, endpoint, caller, timeout, now)
    }

    async fn start_resolved(
        &mut self,
        key: CallKey,
        endpoint: String,
        caller: String,
        timeout: Option<u32>,
        resolved_peer: Option<SocketAddr>,
        now: u64,
    ) -> Result<(), ControlError> {
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
            >= 64
        {
            return Err(ControlError::Capacity);
        }
        let name = endpoint
            .strip_prefix("PJSIP/")
            .ok_or(ControlError::Unsupported)?;
        let endpoint = self
            .config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == name)
            .ok_or(ControlError::NotFound("Endpoint not found"))?;
        let listener = self
            .listeners
            .iter()
            .position(|listener| listener.name == endpoint.transport)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        let transport = self
            .config
            .transports
            .iter()
            .find(|transport| transport.name == endpoint.transport)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        let bind = if transport.bind.ip().is_unspecified() {
            let peer = media::destination_peer(
                &self.listeners[listener].engine,
                &endpoint.name,
                resolved_peer,
                now,
            )?;
            media::local_ip(peer)?
        } else {
            transport.bind.ip()
        };
        let sockets =
            SocketPair::bind(bind, self.config.rtp.start, self.config.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let local_rtp = SocketAddr::new(
            transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        );
        let dtmf = endpoint.dtmf;
        let outbound = Outbound {
            id: key.id.as_str().into(),
            generation: key.generation,
            endpoint: endpoint.name.clone(),
            caller,
            local_rtp,
            timeout_ms: timeout.map(|timeout| u64::from(timeout) * 1000),
        };
        let engine = &mut self.listeners[listener].engine;
        let (pending, output) = if endpoint.webrtc {
            let offer = crate::webrtc::WebRtcOffer::new(
                sockets,
                local_rtp,
                &endpoint.codecs,
                Instant::now(),
            )
            .map_err(failure)?;
            let description =
                rusterisk_sip::WebRtcDescription::parse(offer.description()).map_err(failure)?;
            let output = match resolved_peer {
                Some(peer) => engine.originate_to_with_sdp(outbound, peer, &description, now),
                None => engine.originate_with_sdp(outbound, &description, now),
            }
            .map_err(failure)?;
            (PendingMedia::WebRtc(PendingWebRtc::Offer(offer)), output)
        } else {
            let output = match resolved_peer {
                Some(peer) => engine.originate_to(outbound, peer, now),
                None => engine.originate(outbound, now),
            }
            .map_err(failure)?;
            (PendingMedia::Rtp(sockets), output)
        };
        self.calls.insert(
            key,
            Active {
                listener,
                pending: Some(pending),
                digits: DigitDelivery::new(dtmf),
                direction_reply: None,
            },
        );
        self.output(listener, output, now).await
    }

    async fn effect(&mut self, effect: Effect, now: u64) -> Result<(), ControlError> {
        match effect {
            Effect::ConnectWebSocket {
                key,
                client,
                options,
                channel_variables,
            } => {
                if let Err(error) =
                    self.start_websocket(key.clone(), client, options, channel_variables)
                {
                    self.notify(Signal::WebSocketAdmitted {
                        key,
                        result: Err(error),
                    })
                    .await?;
                }
            }
            Effect::ReserveWebSocket { key } => {
                self.external_media.insert(key.clone());
                self.notify(Signal::DtmfReady {
                    key,
                    events: u16::MAX,
                })
                .await?;
            }
            Effect::ConnectAudioSocket { key, target } => {
                if let Err(error) = self.start_audio_socket(key.clone(), target) {
                    self.notify(Signal::AudioSocketAdmitted {
                        key,
                        result: Err(error),
                    })
                    .await?;
                }
            }
            Effect::ConnectExternalRtp { key, target } => {
                let result = self.connect_external_rtp(key.clone(), target).await;
                if let Err(operation) = &result {
                    self.external_media.remove(&key);
                    if let Err(cleanup) = self.media.release(key.clone()) {
                        return Err(ControlError::Adapter(format!("External media admission failed: {operation}, cleanup failed: {cleanup}")));
                    }
                }
                let admitted = result.is_ok();
                self.notify(Signal::ExternalRtpAdmitted {
                    key: key.clone(),
                    result,
                })
                .await?;
                if admitted {
                    self.notify(Signal::Milestone {
                        key,
                        milestone: Milestone::Answered,
                    })
                    .await?;
                }
            }
            Effect::RecordMedia {
                owner,
                token,
                name,
                options,
            } => {
                self.start_recording(owner, token, name, options, now)
                    .await?;
            }
            Effect::ControlRecording { token, operation } => {
                if let Err(error) = self.media.control_recording(token, operation).await {
                    tracing::warn!(?token, %error, "recording control failed");
                }
            }
            Effect::EndRecording { token, mode } => {
                self.media.stop_recording(token, mode).map_err(failure)?
            }
            Effect::Dtmf { key, command } => {
                if self.external_media.contains(&key)
                    || self
                        .calls
                        .get(&key)
                        .is_some_and(|call| matches!(call.digits, DigitDelivery::Media))
                {
                    match self.media.send_sequence(key.clone(), command).await {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            tracing::warn!(channel = %key.id, generation = key.generation, %error, "digit command admission failed")
                        }
                    }
                    self.notify(Signal::DtmfFinished {
                        key,
                        token: command.token,
                    })
                    .await?;
                    return Ok(());
                }
                if let Some(queue) = self
                    .calls
                    .get_mut(&key)
                    .and_then(|call| call.digits.queue_mut())
                {
                    if queue.iter().any(|pending| pending.token == command.token) {
                        return Ok(());
                    }
                    if queue.len() < 8 {
                        queue.push_back(command);
                        return Ok(());
                    }
                    tracing::warn!(channel = %key.id, generation = key.generation, "digit command queue full");
                }
                self.notify(Signal::DtmfFinished {
                    key,
                    token: command.token,
                })
                .await?;
            }
            Effect::Gate { key, gate } => {
                let gate = match gate {
                    AudioGate::Open => Gate::Open,
                    AudioGate::IncomingMuted => Gate::IncomingMuted,
                    AudioGate::OutgoingMuted => Gate::OutgoingMuted,
                    AudioGate::Closed => Gate::Closed,
                };
                self.media.gate(key, gate).map_err(failure)?;
            }
            Effect::PeerDirection {
                key,
                direction,
                gate,
            } => {
                let gate = match gate {
                    AudioGate::Open => Gate::Open,
                    AudioGate::IncomingMuted => Gate::IncomingMuted,
                    AudioGate::OutgoingMuted => Gate::OutgoingMuted,
                    AudioGate::Closed => Gate::Closed,
                };
                let applied = self
                    .media
                    .peer_direction(key.clone(), direction, gate)
                    .await
                    .map_err(failure)?;
                if let Some(call) = self.calls.get_mut(&key) {
                    if call
                        .direction_reply
                        .as_ref()
                        .is_some_and(|reply| reply.direction == direction)
                    {
                        let reply = call.direction_reply.take().ok_or(ControlError::Capacity)?;
                        if applied {
                            if !self.network.send_deferred(reply.transmission).await? {
                                self.notify(Signal::Terminated { key, cause: 41 }).await?;
                            }
                        } else {
                            self.notify(Signal::Terminated { key, cause: 21 }).await?;
                        }
                    }
                }
            }
            Effect::Answer { key } => {
                if let Err(error) = self.answer(&key, now).await {
                    tracing::warn!(channel = %key.id, generation = key.generation, %error, "incoming answer failed");
                    self.notify(Signal::Terminated { key, cause: 21 }).await?;
                }
            }
            Effect::Ring { key } => {
                if let Some(call) = self.calls.get(&key) {
                    let listener = call.listener;
                    if self.listeners[listener]
                        .engine
                        .pending(key.id.as_str())
                        .is_some()
                    {
                        let output = self.listeners[listener]
                            .engine
                            .ring(key.id.as_str())
                            .map_err(failure)?;
                        self.transmit(listener, output.datagrams).await?;
                    }
                }
            }
            Effect::Start {
                key,
                endpoint,
                caller,
                timeout,
            } => {
                if let Err(error) = self
                    .start(key.clone(), endpoint, caller, timeout, now)
                    .await
                {
                    tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "outgoing call failed");
                    self.notify(Signal::Terminated { key, cause: 21 }).await?;
                }
            }
            Effect::Playback { key, token, media } => {
                let media = rusterisk_api::SoundId::try_from(media)
                    .map(PlaybackMedia::Sound)
                    .map_err(|_| ControlError::Invalid("Invalid sound name"));
                self.start_playback(MediaOwner::Channel(key), token, media)
                    .await?;
            }
            Effect::PlayMedia {
                owner,
                token,
                media,
                language,
            } => {
                let media = match media {
                    MediaUri::Sound(name) if !language.is_empty() => ChannelId::try_from(language)
                        .map(|language| PlaybackMedia::LocalizedSound { name, language })
                        .map_err(ControlError::from),
                    MediaUri::Sound(name) => Ok(PlaybackMedia::Sound(name)),
                    MediaUri::Recording(name) => Ok(PlaybackMedia::Recording(name)),
                };
                self.start_playback(owner, token, media).await?;
            }
            Effect::ControlPlayback { token, operation } => self
                .media
                .control_playback(token, operation)
                .map_err(failure)?,
            Effect::Release { key } => {
                self.cancel_native(&key);
                if let Some(pending) = self.websocket_admissions.remove(&key) {
                    pending.cancel.send_replace(true);
                }
                if let Some(pending) = self.stream_admissions.remove(&key) {
                    pending.cancel.send_replace(true);
                }
                self.external_media.remove(&key);
                if let Some(call) = self.calls.remove(&key) {
                    let output = self.terminate(call.listener, &key, now).map_err(failure)?;
                    self.transmit(call.listener, output.datagrams).await?;
                }
                self.media.release(key).map_err(failure)?;
            }
            Effect::Bridge {
                id,
                generation,
                channels,
            } => {
                tracing::debug!(bridge = %id, generation, "bridge membership changed");
                self.media.bridge(generation, channels).map_err(failure)?;
            }
            Effect::ReleaseBridge { generation } => {
                self.media.release_bridge(generation).map_err(failure)?
            }
            Effect::DialBridge {
                parent,
                generation,
                peer,
            } => match peer {
                Some(peer) => self
                    .media
                    .bridge(generation, [parent, peer].into_iter().collect())
                    .map_err(failure)?,
                None => self.media.release_bridge(generation).map_err(failure)?,
            },
        }
        Ok(())
    }

    async fn start_playback(
        &mut self,
        owner: MediaOwner,
        token: MediaTaskToken,
        media: Result<PlaybackMedia, ControlError>,
    ) -> Result<(), ControlError> {
        let available = match &owner {
            MediaOwner::Channel(key) => {
                self.calls.contains_key(key) || self.external_media.contains(key)
            }
            MediaOwner::Bridge { .. } => true,
        };
        match &self.storage {
            Some(storage) if self.playbacks.len() < 64 && available => {
                let storage = storage.clone();
                let port = self.media.clone();
                let controller = self.controller.clone();
                let completed = match port
                    .reserve_playback(
                        match &owner {
                            MediaOwner::Channel(key) => AudioTarget::Channel(key.clone()),
                            MediaOwner::Bridge { generation, .. } => {
                                AudioTarget::Bridge(*generation)
                            }
                        },
                        token,
                    )
                    .await
                {
                    Ok(completed) => completed,
                    Err(error) => {
                        tracing::warn!(?owner, ?token, %error, "playback reservation failed");
                        self.notify(Signal::MediaPlaybackFinished {
                            owner,
                            token,
                            completion: crate::PlaybackCompletion::Failed,
                        })
                        .await?;
                        return Ok(());
                    }
                };
                self.playbacks.spawn(async move {
                        let result = async {
                            let source = storage
                                .play(media?)
                                .await
                                .map_err(failure)?;
                            port
                                .attach_playback(token, source)
                                .await
                                .map_err(failure)?;
                            controller.signal(Signal::MediaPlaybackStarted { owner: owner.clone(), token }).await?;
                            completed.await.map_err(failure)?.map_err(failure)
                        }
                        .await;
                        if result.is_err() {
                            if let Err(error) = port.stop_playback(token) {
                                tracing::warn!(?owner, ?token, %error, "failed playback reservation cleanup deferred");
                            }
                        }
                        (owner, token, result)
                    });
            }
            Some(_) | None => {
                tracing::warn!(?owner, ?token, "playback admission failed");
                self.notify(Signal::MediaPlaybackFinished {
                    owner,
                    token,
                    completion: crate::PlaybackCompletion::Failed,
                })
                .await?;
            }
        }
        Ok(())
    }

    async fn answer(&mut self, key: &CallKey, now: u64) -> Result<(), ControlError> {
        if self.external_media.contains(key) {
            return Ok(());
        }
        let call = self
            .calls
            .get(key)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        let listener = call.listener;
        if self.listeners[listener]
            .engine
            .active_session(key.id.as_str(), key.generation)
            .is_some()
        {
            return Ok(());
        }
        let transport = self
            .config
            .transports
            .iter()
            .find(|transport| transport.name == self.listeners[listener].name)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        let incoming = self.listeners[listener]
            .engine
            .pending(key.id.as_str())
            .ok_or(ControlError::NotFound("Incoming offer not found"))?;
        let offered = incoming.session.clone();
        let bind = if transport.bind.ip().is_unspecified() {
            media::local_ip(incoming.peer)?
        } else {
            transport.bind.ip()
        };
        let sockets =
            SocketPair::bind(bind, self.config.rtp.start, self.config.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let advertised = SocketAddr::new(
            transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        );
        let (pending, output) = match &offered.security {
            rusterisk_sip::SessionSecurity::Rtp => {
                let output = self.listeners[listener]
                    .engine
                    .answer(key.id.as_str(), key.generation, advertised, now)
                    .map_err(failure)?;
                (PendingMedia::Rtp(sockets), output)
            }
            rusterisk_sip::SessionSecurity::WebRtc(description) => {
                let (session, answer) = crate::webrtc::WebRtcSession::answer(
                    sockets,
                    advertised,
                    description.as_str(),
                    &[offered.codec],
                    Instant::now(),
                )
                .map_err(failure)?;
                let answer = rusterisk_sip::WebRtcDescription::parse(&answer).map_err(failure)?;
                let output = self.listeners[listener]
                    .engine
                    .answer_with_sdp(key.id.as_str(), key.generation, advertised, &answer, now)
                    .map_err(failure)?;
                (
                    PendingMedia::WebRtc(PendingWebRtc::Session(session)),
                    output,
                )
            }
        };
        self.calls
            .get_mut(key)
            .ok_or(ControlError::NotFound("Channel not found"))?
            .pending = Some(pending);
        self.output(listener, output, now).await
    }

    fn terminate(
        &mut self,
        listener: usize,
        key: &CallKey,
        now: u64,
    ) -> Result<Output, rusterisk_sip::SipError> {
        let engine = &mut self.listeners[listener].engine;
        if engine.pending(key.id.as_str()).is_some() {
            engine.reject(key.id.as_str(), RejectReason::Rejected, now)
        } else {
            engine.cancel(key.id.as_str(), key.generation, now)
        }
    }

    async fn digit_tick(&mut self, now: u64) -> Result<(), ControlError> {
        let keys: Vec<_> = self.calls.keys().cloned().collect();
        for key in keys {
            let Some(call) = self.calls.get_mut(&key) else {
                continue;
            };
            let Some(command) = call.digits.queue_mut().and_then(|queue| queue.front_mut()) else {
                continue;
            };
            let tone = command.sequence.advance(now).map_err(ControlError::from)?;
            let mut finished = command.sequence.finished();
            let token = command.token;
            let listener = call.listener;
            if let Some(tone) = tone {
                let result = match self.listeners[listener].engine.send_info(
                    key.id.as_str(),
                    key.generation,
                    tone.digit,
                    tone.duration_ms,
                    now,
                ) {
                    Ok(output) => self.transmit(listener, output.datagrams).await,
                    Err(error) => Err(failure(error)),
                };
                if let Err(error) = result {
                    tracing::warn!(channel = %key.id, generation = key.generation, %error, "digit command failed");
                    finished = true;
                }
            }
            if finished {
                self.calls
                    .get_mut(&key)
                    .ok_or(ControlError::Capacity)?
                    .digits
                    .queue_mut()
                    .ok_or(ControlError::Conflict("Signaling owns digit timing"))?
                    .pop_front();
                self.notify(Signal::DtmfFinished { key, token }).await?;
            }
        }
        Ok(())
    }

    async fn serve(
        &mut self,
        stop: &mut watch::Receiver<bool>,
        started: Instant,
    ) -> Result<(), ControlError> {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                effect = self.effects.recv() => match effect {
                    Some(effect) => {
                        let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                        if let Err(error) = self.effect(effect, now).await { if *stop.borrow() { break; } return Err(error); }
                    }
                    None => break,
                },
                packet = self.network.next() => {
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    match packet? {
                        transport::Input::Packet(packet) => match self.listeners[packet.listener].engine.receive(&packet.bytes, packet.peer, now) {
                            Ok(output) => self.output(packet.listener, output, now).await?,
                            Err(error) => {
                                self.network.refuse(packet.listener, packet.peer);
                                tracing::debug!(transport = %self.listeners[packet.listener].name, %error, "signaling input refused");
                            }
                        },
                        transport::Input::Closed { key, result } => {
                            if let Err(error) = result { tracing::debug!(transport = %self.listeners[key.listener].name, %error, "signaling stream closed"); }
                            let output = self.listeners[key.listener].engine.disconnected(key.peer);
                            self.output(key.listener, output, now).await?;
                        },
                        transport::Input::Prepared(prepared) => self.native_prepared(prepared, now).await?,
                        transport::Input::SetupFinished { key, result } => self.native_setup_finished(key, result).await?,
                    }
                }
                notice = self.notices.recv() => match notice {
                    Some(MediaNotice::Answer { key }) => {
                        if let Some(pending) = self.websocket_admissions.get_mut(&key) { pending.answered = true; }
                        else { self.notify(Signal::Milestone { key, milestone: Milestone::Answered }).await?; }
                    },
                    Some(MediaNotice::DtmfFinished { key, token }) => self.notify(Signal::DtmfFinished { key, token }).await?,
                    Some(MediaNotice::Dtmf { key, tone }) => self.notify(Signal::DtmfReceived { key, digit: tone.digit, duration_ms: tone.duration_ms }).await?,
                    Some(MediaNotice::Connected { key }) => {
                        if let Some(pending) = self.stream_admissions.get_mut(&key) { pending.received = true; }
                        else if let Some(pending) = self.websocket_admissions.get_mut(&key) { pending.received = true; }
                        else { self.notify(Signal::Milestone { key, milestone: Milestone::MediaConnected }).await?; }
                    },
                    Some(MediaNotice::Stopped { key, result }) => {
                        if let Some(pending) = self.websocket_admissions.remove(&key) {
                            pending.cancel.send_replace(true);
                            self.notify(Signal::WebSocketAdmitted { key: key.clone(), result: Err(ControlError::Adapter("WebSocket ended during admission".into())) }).await?;
                        }
                        if let Some(pending) = self.stream_admissions.remove(&key) {
                            pending.cancel.send_replace(true);
                            self.notify(Signal::AudioSocketAdmitted { key: key.clone(), result: Err(ControlError::Adapter("AudioSocket ended during admission".into())) }).await?;
                        }
                        self.external_media.remove(&key);
                        if let Err(error) = result { tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "media stopped"); }
                        self.notify(Signal::Terminated { key, cause: 16 }).await?;
                    }
                    None => { if !*stop.borrow() { return Err(ControlError::Adapter("Media task stopped".into())); } break; }
                },
                result = self.playbacks.join_next(), if !self.playbacks.is_empty() => {
                    let (owner, token, result) = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    let completion = match result {
                        Ok(()) => crate::PlaybackCompletion::Completed,
                        Err(error) => {
                            tracing::warn!(?owner, ?token, %error, "playback failed");
                            crate::PlaybackCompletion::Failed
                        }
                    };
                    self.notify(Signal::MediaPlaybackFinished { owner, token, completion }).await?;
                },
                result = self.recordings.join_next(), if !self.recordings.is_empty() => {
                    let completed = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    self.recording_completed(completed).await?;
                },
                result = self.stream_jobs.join_next(), if !self.stream_jobs.is_empty() => {
                    let completed = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    self.audio_socket_completed(completed).await?;
                },
                result = self.websocket_jobs.join_next(), if !self.websocket_jobs.is_empty() => {
                    let completed = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    self.websocket_completed(completed).await?;
                },
                result = self.native_jobs.join_next(), if !self.native_jobs.is_empty() => {
                    let completed = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    self.native_completed(completed, now).await?;
                },
                _instant = tick.tick() => {
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    for listener in 0..self.listeners.len() {
                        let output = self.listeners[listener].engine.tick(now).map_err(failure)?;
                        self.output(listener, output, now).await?;
                    }
                    self.digit_tick(now).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn run(mut self, mut stop: watch::Receiver<bool>) -> Result<(), ControlError> {
        let started = Instant::now();
        let result = self.serve(&mut stop, started).await;
        let mut cleanup = Ok(());
        let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
        if let Err(error) = self.finish_native().await {
            cleanup = Err(error);
        }
        if let Err(error) = self.finish_streams().await {
            cleanup = Err(error);
        }
        if let Err(error) = self.finish_websockets().await {
            cleanup = Err(error);
        }
        for (key, call) in std::mem::take(&mut self.calls) {
            match self.terminate(call.listener, &key, now) {
                Ok(output) => {
                    if let Err(error) = self.transmit(call.listener, output.datagrams).await {
                        tracing::error!(%error, "signaling cleanup failed");
                        cleanup = Err(error);
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "dialog cleanup failed");
                    cleanup = Err(failure(error));
                }
            }
        }
        self.playbacks.abort_all();
        while let Some(result) = self.playbacks.join_next().await {
            match result {
                Ok((owner, token, Err(error))) => {
                    tracing::warn!(?owner, ?token, %error, "playback failed before shutdown")
                }
                Ok((_, _, Ok(()))) => {}
                Err(error) if error.is_cancelled() => {
                    tracing::debug!(%error, "playback cancelled during shutdown")
                }
                Err(error) => {
                    tracing::error!(%error, "playback task failed during shutdown");
                    cleanup = Err(failure(error));
                }
            }
        }
        if let Err(error) = self.finish_recordings().await {
            tracing::error!(%error, "recording shutdown failed");
            cleanup = Err(error);
        }
        if let Err(error) = self.network.close().await {
            tracing::error!(%error, "signaling stream shutdown failed");
            cleanup = Err(error);
        }
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; signaling cleanup failed: {cleanup}"
            ))),
        }
    }
}
