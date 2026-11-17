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
use rusterisk_sip::{
    Datagram, DatagramOrigin, Engine, Outbound, Output, RejectReason, SipEvent, StreamFlowId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
mod attended;
mod cleanup;
mod early_media;
mod external_media;
mod media;
use media::{AnsweredAttachment, MediaAdmission, PendingWebRtc, PreparedAnswer};
#[cfg(test)]
mod negotiation_tests;
mod outbound;
#[cfg(test)]
mod playback_admission_tests;
mod publication;
pub(crate) use publication::{NativePreparation, NativePreparedSignaling};
mod recording;
mod refer_routes;
mod registrations;
mod resolver;
pub(crate) mod sip_websocket;
#[cfg(test)]
mod stream_tests;
mod transport;
pub use sip_websocket::SipWebSocketPort;
#[cfg(test)]
mod sip_websocket_tests;
mod websocket;
pub(crate) use websocket::{PreparedClients as PreparedWebSocketProfiles, PreparedWebSocketClient};

struct Listener {
    name: String,
    engine: Engine,
}
struct Active {
    listener: usize,
    media: MediaAdmission,
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
    route: transport::DeliveryRoute,
    bytes: Vec<u8>,
}

impl Received {
    fn route(&self) -> transport::DeliveryRoute {
        self.route
    }
}

type PlaybackResult = (MediaOwner, MediaTaskToken, Result<(), ControlError>);

pub struct Signaling {
    config: Config,
    native_profiles: crate::call_profiles::BoundNativeProfiles,
    startup_native_profiles: crate::call_profiles::BoundNativeProfiles,
    publication: Option<publication::NativePublication>,
    controller: Controller,
    effects: mpsc::Receiver<Effect>,
    media: MediaPort,
    notices: mpsc::Receiver<MediaNotice>,
    listeners: Vec<Listener>,
    network: transport::Network,
    registrations: registrations::Registrations,
    native_pending: BTreeMap<CallKey, outbound::Pending>,
    native_jobs: JoinSet<outbound::Completion>,
    resolver: Result<resolver::DestinationResolver, ControlError>,
    resolution_cache: resolver::ResolutionCache,
    calls: BTreeMap<CallKey, Active>,
    refer_routes: BTreeMap<(usize, rusterisk_sip::ReferKey), refer_routes::ReferRoute>,
    external_media: BTreeSet<CallKey>,
    stream_admissions: BTreeMap<CallKey, external_media::PendingStream>,
    stream_jobs: JoinSet<external_media::StreamCompletion>,
    websocket_admissions: BTreeMap<CallKey, websocket::PendingWebSocket>,
    websocket_jobs: JoinSet<websocket::Completion>,
    websocket_clients: websocket::PreparedClients,
    storage: Option<StoragePort>,
    playbacks: JoinSet<PlaybackResult>,
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

fn audio_gate(gate: AudioGate) -> Gate {
    match gate {
        AudioGate::Open => Gate::Open,
        AudioGate::IncomingMuted => Gate::IncomingMuted,
        AudioGate::OutgoingMuted => Gate::OutgoingMuted,
        AudioGate::Closed => Gate::Closed,
    }
}

impl Signaling {
    pub fn websocket_port(&self) -> SipWebSocketPort {
        self.network.websocket_port()
    }

    pub fn with_storage(mut self, storage: StoragePort) -> Self {
        self.storage = Some(storage);
        self
    }

    pub(crate) fn native_profiles(&self) -> crate::call_profiles::BoundNativeProfiles {
        self.native_profiles.clone()
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
        let registrations = registrations::Registrations::new(&config)?;
        let websocket_clients = websocket::PreparedClients::new(&config.websocket.clients)?;
        let requested = config.transports.clone();
        let network = transport::Network::bind(&mut config).await?;
        let native_profiles = network.native_profiles(&requested)?;
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
        controller.bind_native_profiles(native_profiles.clone())?;
        controller.bind_websocket_profiles(websocket_clients.clone())?;
        Ok(Self {
            config,
            startup_native_profiles: native_profiles.clone(),
            native_profiles,
            publication: None,
            controller,
            effects,
            media,
            notices,
            listeners,
            network,
            registrations,
            native_pending: BTreeMap::new(),
            native_jobs: JoinSet::new(),
            resolver: resolver::DestinationResolver::system(),
            resolution_cache: resolver::ResolutionCache::default(),
            calls: BTreeMap::new(),
            refer_routes: BTreeMap::new(),
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
            match packet.origin() {
                DatagramOrigin::Direct => {
                    if !self.network.send_packet(listener, packet).await? {
                        tracing::debug!("signaling delivery flow is closed");
                    }
                }
                DatagramOrigin::Refer(reference) => {
                    if let Some(route) = self.refer_delivery(listener, reference) {
                        if self.network.send_on(route, packet).await? {
                            continue;
                        }
                    }
                    self.cancel_refer_wire(listener, reference)?;
                }
            }
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
        self.prune_refer_routes(listener, now)?;
        let settings = self.registrations.settings_epoch;
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
                SipEvent::AttendedTransferRequested {
                    id,
                    generation,
                    refer,
                } => {
                    self.admit_attended_refer(listener, id, generation, refer, now)
                        .await?;
                }
                SipEvent::BlindTransferRequested {
                    id,
                    generation,
                    refer,
                } => {
                    self.admit_refer(listener, id, generation, refer, now)
                        .await?;
                }
                SipEvent::ReferSubscriptionEnded { refer } => self.settle_refer(listener, refer),
                SipEvent::ForkTeardownFailed {
                    id,
                    generation,
                    failure,
                } => {
                    tracing::warn!(channel = %id, generation, ?failure, "forked SIP dialog teardown failed");
                }
                SipEvent::EndpointContact {
                    endpoint,
                    generation,
                    presence,
                } => match settings {
                    Some(settings) => {
                        match self
                            .controller
                            .observe_endpoint(settings, endpoint, presence)
                            .await
                        {
                            Ok(()) => {}
                            Err(ControlError::Conflict(_) | ControlError::NotFound(_)) => {
                                tracing::debug!("stale endpoint contact observation rejected");
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    None => {
                        self.notify(Signal::EndpointContact {
                            key: crate::EndpointKey {
                                resource: endpoint,
                                generation,
                            },
                            presence,
                        })
                        .await?;
                    }
                },
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
                    let attached = match self.queue_answered_attachment(&key, &session) {
                        Ok(AnsweredAttachment::Existing) => Some(Ok(())),
                        Ok(AnsweredAttachment::Pending(receipt)) => Some(receipt.wait().await),
                        Ok(AnsweredAttachment::Missing) => None,
                        Err(error) => Some(Err(error)),
                    };
                    if let Some(attached) = attached {
                        let direction = peer_direction(session.direction);
                        match attached {
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
                    match self.admit(listener, incoming).await {
                        Ok((key, direction)) => {
                            self.notify(Signal::PeerDirection { key, direction })
                                .await?
                        }
                        Err(error) => {
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
                                | ControlError::Configuration(_)
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

    async fn admit(
        &mut self,
        listener: usize,
        incoming: IncomingCall,
    ) -> Result<(CallKey, PeerMediaDirection), ControlError> {
        if self.calls.len()
            + self.external_media.len()
            + self.stream_admissions.len()
            + self.websocket_admissions.len()
            >= 64
        {
            return Err(ControlError::Capacity);
        }
        if !self.admitting() {
            return Err(ControlError::Capacity);
        }
        let profile = crate::call_profiles::NativeMediaProfile::capture(
            &self.config,
            &incoming.endpoint,
            &self.native_profiles,
        )?;
        let dtmf = profile.dtmf;
        let direction = peer_direction(
            self.listeners[listener]
                .engine
                .pending(incoming.id.as_str())
                .ok_or(ControlError::NotFound("Incoming offer not found"))?
                .session
                .direction,
        );
        let key = self.controller.incoming(incoming).await?;
        self.calls.insert(
            key.clone(),
            Active {
                listener,
                media: MediaAdmission::Incoming(profile),
                digits: DigitDelivery::new(dtmf),
                direction_reply: None,
            },
        );
        Ok((key, direction))
    }

    async fn start(
        &mut self,
        key: CallKey,
        endpoint: String,
        call: outbound::CallStart,
        profile: crate::NativeCallProfile,
        settings: rusterisk_core::SettingsGeneration,
        now: u64,
    ) -> Result<(), ControlError> {
        let epoch = self
            .registrations
            .settings_epoch
            .map(|_| transport::ProfileEpoch::Settings(settings))
            .unwrap_or(transport::ProfileEpoch::Configuration(
                self.registrations.generation(),
            ));
        self.queue_native_profile(key, endpoint, call, profile, epoch, now)
    }

    async fn start_resolved(
        &mut self,
        key: CallKey,
        native: outbound::NativeStart,
        call: outbound::CallStart,
        resolved_peer: Option<SocketAddr>,
        flow: Option<StreamFlowId>,
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
        let outbound::NativeStart {
            endpoint,
            prepared,
            media: profile,
            binding,
        } = native;
        let listener = binding.listener();
        let bind = if profile.bind_ip().is_unspecified() {
            let peer = match (prepared.destination(), resolved_peer) {
                (rusterisk_sip::Destination::Registered(peer), None) => *peer,
                (rusterisk_sip::Destination::Static(_), Some(peer)) => peer,
                (rusterisk_sip::Destination::Registered(_), Some(_))
                | (rusterisk_sip::Destination::Static(_), None) => {
                    return Err(ControlError::Invalid("Signaling destination changed"))
                }
            };
            media::local_ip(peer)?
        } else {
            profile.bind_ip()
        };
        let sockets =
            SocketPair::bind(bind, profile.rtp.start, profile.rtp.end).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let local_rtp = profile.advertised(local);
        let dtmf = profile.dtmf;
        let outbound::CallStart { caller, timeout } = call;
        let outbound = Outbound {
            id: key.id.as_str().into(),
            generation: key.generation,
            endpoint,
            caller,
            local_rtp,
            timeout_ms: timeout.map(|timeout| u64::from(timeout) * 1000),
        };
        let engine = &mut self.listeners[listener].engine;
        let (pending, output) = if profile.webrtc {
            let offer = crate::webrtc::WebRtcOffer::new(
                sockets,
                local_rtp,
                &profile.codecs,
                Instant::now(),
            )
            .map_err(failure)?;
            let description =
                rusterisk_sip::WebRtcDescription::parse(offer.description()).map_err(failure)?;
            let output = match flow {
                Some(flow) => engine.originate_prepared_on_flow(
                    outbound,
                    prepared,
                    resolved_peer,
                    Some(&description),
                    flow,
                    now,
                ),
                None => engine.originate_prepared(
                    outbound,
                    prepared,
                    resolved_peer,
                    Some(&description),
                    now,
                ),
            }
            .map_err(failure)?;
            (MediaAdmission::WebRtc(PendingWebRtc::Offer(offer)), output)
        } else {
            let output = match flow {
                Some(flow) => engine.originate_prepared_on_flow(
                    outbound,
                    prepared,
                    resolved_peer,
                    None,
                    flow,
                    now,
                ),
                None => engine.originate_prepared(outbound, prepared, resolved_peer, None, now),
            }
            .map_err(failure)?;
            (MediaAdmission::Rtp(sockets), output)
        };
        self.calls.insert(
            key,
            Active {
                listener,
                media: pending,
                digits: DigitDelivery::new(dtmf),
                direction_reply: None,
            },
        );
        self.output(listener, output, now).await
    }

    async fn effect(&mut self, effect: Effect, now: u64) -> Result<(), ControlError> {
        match effect {
            Effect::RejectRefer {
                source,
                refer,
                failure,
            } => self.reject_refer(&source, refer, failure, now).await?,
            Effect::ReferProgress {
                source,
                refer,
                token,
                progress,
            } => {
                self.progress_refer(&source, refer, token, progress, now)
                    .await?
            }
            Effect::ConnectWebSocket {
                key,
                settings,
                client: _,
                options,
                channel_variables,
            } => {
                let prepared = self
                    .controller
                    .take_websocket_profile(key.clone(), settings)
                    .await;
                let started = match prepared {
                    Ok(prepared) => {
                        self.start_websocket(key.clone(), prepared, options, channel_variables)
                    }
                    Err(ControlError::Conflict(_) | ControlError::NotFound(_)) => {
                        tracing::debug!(channel = %key.id, generation = key.generation, "WebSocket start key rejected");
                        return Ok(());
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = started {
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
            Effect::Gate {
                key,
                gate,
                generated_audio_gate,
            } => {
                self.media
                    .gate_with_generated_audio(
                        key,
                        audio_gate(gate),
                        audio_gate(generated_audio_gate),
                    )
                    .map_err(failure)?;
            }
            Effect::PeerDirection {
                key,
                direction,
                gate,
                generated_audio_gate,
            } => {
                let applied = self
                    .media
                    .peer_direction_with_generated_audio(
                        key.clone(),
                        direction,
                        audio_gate(gate),
                        audio_gate(generated_audio_gate),
                    )
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
                            .ring_at(key.id.as_str(), now)
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
                settings,
            } => {
                if self.calls.contains_key(&key) || self.native_pending.contains_key(&key) {
                    return Ok(());
                }
                let capsule = self
                    .controller
                    .take_native_profile(key.clone(), settings)
                    .await;
                let profile = match capsule {
                    Ok(capsule) => capsule.into_profile(),
                    Err(ControlError::Conflict(_) | ControlError::NotFound(_)) => {
                        tracing::debug!(channel = %key.id, generation = key.generation, "native start key rejected");
                        return Ok(());
                    }
                    Err(error) => Err(error),
                };
                let started = match profile {
                    Ok(profile) => {
                        self.start(
                            key.clone(),
                            endpoint,
                            outbound::CallStart { caller, timeout },
                            profile,
                            settings,
                            now,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = started {
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
                options,
            } => {
                let media = match media {
                    MediaUri::Sound(name) if !language.is_empty() => ChannelId::try_from(language)
                        .map(|language| PlaybackMedia::LocalizedSound { name, language })
                        .map_err(ControlError::from),
                    MediaUri::Sound(name) => Ok(PlaybackMedia::Sound(name)),
                    MediaUri::Recording(name) => Ok(PlaybackMedia::Recording(name)),
                };
                self.start_playback_with_options(owner, token, media, options)
                    .await?;
            }
            Effect::StartMusic {
                key,
                token,
                selection,
            } => {
                self.start_music(key, token, selection).await?;
            }
            Effect::PrepareEarlyMusic { key, token } => {
                let result = self.prepare_early_media(&key, now).await;
                if let Err(error) = &result {
                    tracing::warn!(channel = %key.id, generation = key.generation, ?token, %error, "early media preparation failed");
                }
                self.notify(Signal::EarlyMediaPrepared { key, token, result })
                    .await?;
            }
            Effect::MusicClassUnavailable { key } => {
                tracing::warn!(channel = %key.id, generation = key.generation, "music class lookup failed; current source retained");
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
        self.start_playback_with_options(owner, token, media, Default::default())
            .await
    }

    async fn music_failed(
        &self,
        owner: MediaOwner,
        token: MediaTaskToken,
        error: ControlError,
    ) -> Result<(), ControlError> {
        tracing::warn!(?owner, ?token, %error, "music source admission failed");
        self.notify(Signal::MediaPlaybackFinished {
            owner,
            token,
            completion: crate::PlaybackCompletion::Failed,
        })
        .await
    }

    async fn playback_completed(
        &self,
        (owner, token, result): PlaybackResult,
    ) -> Result<(), ControlError> {
        let completion = match result {
            Ok(()) => crate::PlaybackCompletion::Completed,
            Err(error) => {
                tracing::warn!(?owner, ?token, %error, "playback failed");
                crate::PlaybackCompletion::Failed
            }
        };
        self.notify(Signal::MediaPlaybackFinished {
            owner,
            token,
            completion,
        })
        .await
    }

    async fn reap_ready_playback(&mut self) -> Result<(), ControlError> {
        if let Some(completed) = self.playbacks.try_join_next() {
            self.playback_completed(completed.map_err(failure)?).await?;
        }
        Ok(())
    }

    async fn start_music(
        &mut self,
        key: CallKey,
        token: MediaTaskToken,
        selection: Result<crate::storage::MusicSelection, ControlError>,
    ) -> Result<(), ControlError> {
        let owner = MediaOwner::Channel(key.clone());
        let selection = match selection {
            Ok(selection) => selection,
            Err(error) => return self.music_failed(owner, token, error).await,
        };
        if !self.calls.contains_key(&key) && !self.external_media.contains(&key) {
            return self
                .music_failed(
                    owner,
                    token,
                    ControlError::NotFound("Channel media unavailable"),
                )
                .await;
        }
        if self.playbacks.len() >= 64 {
            self.reap_ready_playback().await?;
        }
        if self.playbacks.len() >= 64 {
            return self
                .music_failed(owner, token, ControlError::Capacity)
                .await;
        }
        let Some(storage) = self.storage.clone() else {
            return self
                .music_failed(owner, token, ControlError::Unsupported)
                .await;
        };
        let class = selection.class_name().to_owned();
        let (cursor, completed) = match self.media.reserve_music(key, token, class).await {
            Ok(admission) => admission,
            Err(error) => return self.music_failed(owner, token, failure(error)).await,
        };
        let port = self.media.clone();
        let controller = self.controller.clone();
        self.playbacks.spawn(async move {
            let result = async {
                let source = storage
                    .play_selected_music(selection, cursor)
                    .await
                    .map_err(failure)?;
                port.attach_playback(token, source).await.map_err(failure)?;
                controller
                    .signal(Signal::MediaPlaybackStarted {
                        owner: owner.clone(),
                        token,
                    })
                    .await?;
                completed.await.map_err(failure)?.map_err(failure)
            }
            .await;
            if result.is_err() {
                if let Err(error) = port.stop_playback(token) {
                    tracing::warn!(?owner, ?token, %error, "music reservation cleanup deferred");
                }
            }
            (owner, token, result)
        });
        Ok(())
    }

    async fn start_playback_with_options(
        &mut self,
        owner: MediaOwner,
        token: MediaTaskToken,
        media: Result<PlaybackMedia, ControlError>,
        options: rusterisk_api::PlaybackOptions,
    ) -> Result<(), ControlError> {
        let available = match &owner {
            MediaOwner::Channel(key) => {
                self.calls.contains_key(key) || self.external_media.contains(key)
            }
            MediaOwner::Bridge { .. } => true,
        };
        if self.playbacks.len() >= 64 {
            self.reap_ready_playback().await?;
        }
        match &self.storage {
            Some(storage) if self.playbacks.len() < 64 && available => {
                let storage = storage.clone();
                let port = self.media.clone();
                let controller = self.controller.clone();
                let completed = match port
                    .reserve_playback_with_options(
                        match &owner {
                            MediaOwner::Channel(key) => AudioTarget::Channel(key.clone()),
                            MediaOwner::Bridge { generation, .. } => {
                                AudioTarget::Bridge(*generation)
                            }
                        },
                        token,
                        options,
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
        let Some((listener, output)) = self.incoming_answer(key, now)? else {
            return Ok(());
        };
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
        let publication_stopped = stop.clone();
        loop {
            if *stop.borrow() {
                break;
            }
            let (prepared, commands) = match &mut self.publication {
                Some(publication) => (
                    Some(&mut publication.receiver.prepared),
                    Some(&mut publication.receiver.commands),
                ),
                None => (None, None),
            };
            tokio::select! {
                prepared = publication::receive(prepared, &publication_stopped) => match prepared? {
                    Some(prepared) => self.stage_native_settings(prepared)?,
                    None => break,
                },
                command = publication::receive(commands, &publication_stopped) => match command? {
                    Some(command) => {
                        let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                        self.native_publication(command, now)?;
                    }
                    None => break,
                },
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
                        transport::Input::Upgrade(request) => {
                            let admitting = self.admitting();
                            self.network.admit_upgrade(request, &self.config.origins, admitting);
                        },
                        transport::Input::Packet(packet) => {
                            if self.registration_received(&packet, now) { continue; }
                            let engine = &mut self.listeners[packet.listener].engine;
                            let output = match packet.route().flow()? {
                                Some(flow) => engine.receive_on_flow(&packet.bytes, packet.peer, flow, now),
                                None => engine.receive(&packet.bytes, packet.peer, now),
                            };
                            match output {
                            Ok(output) => self.output(packet.listener, output, now).await?,
                            Err(error) => {
                                self.network.refuse(packet.route());
                                tracing::debug!(transport = %self.listeners[packet.listener].name, %error, "signaling input refused");
                            }
                            }
                        },
                        transport::Input::Closed { key, result } => {
                            self.registration_closed(key);
                            if let Err(error) = result { tracing::debug!(transport = %self.listeners[key.listener].name, %error, "signaling stream closed"); }
                            let output = self.listeners[key.listener].engine.disconnected_flow(key.flow()?);
                            self.output(key.listener, output, now).await?;
                        },
                        transport::Input::Prepared(prepared) => {
                            if self.registration_owns_setup(prepared.key) { self.registration_prepared(prepared, now)?; }
                            else { self.native_prepared(prepared, now).await?; }
                        },
                        transport::Input::SetupFinished { key, result } => {
                            if self.registration_owns_setup(key) { self.registration_setup_finished(key, result); }
                            else { self.native_setup_finished(key, result).await?; }
                        },
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
                    let completed = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    self.playback_completed(completed).await?;
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
                result = self.registrations.completed(), if self.registrations.setup_count() > 0 => {
                    let completed = result?.ok_or(ControlError::Capacity)?;
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    self.registration_completed(completed, now)?;
                },
                _instant = tick.tick() => {
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    for listener in 0..self.listeners.len() {
                        let output = self.listeners[listener].engine.tick(now).map_err(failure)?;
                        self.output(listener, output, now).await?;
                    }
                    self.digit_tick(now).await?;
                    self.registration_tick(now).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn run(mut self, mut stop: watch::Receiver<bool>) -> Result<(), ControlError> {
        let started = Instant::now();
        let result = self.serve(&mut stop, started).await;
        let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
        let mut cleanup = self.finish_registrations().await;
        cleanup = cleanup::combine(cleanup, self.close_refer_routes(now));
        cleanup = cleanup::combine(cleanup, self.finish_native().await);
        cleanup = cleanup::combine(cleanup, self.finish_streams().await);
        cleanup = cleanup::combine(cleanup, self.finish_websockets().await);
        for (key, call) in std::mem::take(&mut self.calls) {
            let listener = call.listener;
            drop(call);
            match self.terminate(listener, &key, now) {
                Ok(output) => {
                    if let Err(error) = self.transmit(listener, output.datagrams).await {
                        tracing::error!(%error, "signaling cleanup failed");
                        cleanup = cleanup::combine(cleanup, Err(error));
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "dialog cleanup failed");
                    cleanup = cleanup::combine(cleanup, Err(failure(error)));
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
                    cleanup = cleanup::combine(cleanup, Err(failure(error)));
                }
            }
        }
        if let Err(error) = self.finish_recordings().await {
            tracing::error!(%error, "recording shutdown failed");
            cleanup = cleanup::combine(cleanup, Err(error));
        }
        if let Err(error) = self.network.close().await {
            tracing::error!(%error, "signaling stream shutdown failed");
            cleanup = cleanup::combine(cleanup, Err(error));
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
