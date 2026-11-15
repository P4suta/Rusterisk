use crate::media::{MediaNotice, MediaPort};
use crate::rtp::{RtpSession, SocketPair};
use crate::storage::{PlaybackMedia, StoragePort};
use crate::{
    CallKey, ControlError, Controller, Effect, IncomingCall, MediaOwner, Milestone, Signal,
};
use rusterisk_api::{ChannelId, MediaUri};
use rusterisk_config::{Codec, Config, Dtmf, Protocol};
use rusterisk_core::{AudioGate, DtmfCommand, MediaTaskToken, PeerMediaDirection};
use rusterisk_media::{AudioFormat, AudioTarget, Gate, TelephoneFormat};
use rusterisk_sip::{Datagram, Engine, Outbound, Output, RejectReason, SipEvent, MAX_MESSAGE};
use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
#[cfg(test)]
mod negotiation_tests;
mod recording;

struct Listener {
    name: String,
    socket: UdpSocket,
    engine: Engine,
}
struct Active {
    listener: usize,
    pending: Option<SocketPair>,
    digits: DigitDelivery,
    direction_reply: Option<DirectionReply>,
}

struct DirectionReply {
    direction: PeerMediaDirection,
    datagram: Datagram,
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
    calls: BTreeMap<CallKey, Active>,
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

async fn receive(listeners: &[Listener]) -> Result<Received, ControlError> {
    std::future::poll_fn(|context| {
        let mut bytes = [0; MAX_MESSAGE + 1];
        for (index, listener) in listeners.iter().enumerate() {
            let mut buffer = ReadBuf::new(&mut bytes);
            match listener.socket.poll_recv_from(context, &mut buffer) {
                Poll::Ready(Ok(peer)) => {
                    return Poll::Ready(Ok(Received {
                        listener: index,
                        peer,
                        bytes: buffer.filled().to_vec(),
                    }))
                }
                Poll::Ready(Err(error)) => return Poll::Ready(Err(failure(error))),
                Poll::Pending => {}
            }
        }
        Poll::Pending
    })
    .await
}

impl Signaling {
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
        if config.transports.len() > 16 {
            return Err(ControlError::Capacity);
        }
        let mut sockets = Vec::with_capacity(config.transports.len());
        for transport in &mut config.transports {
            match transport.protocol {
                Protocol::Udp => {
                    let socket = UdpSocket::bind(transport.bind).await.map_err(failure)?;
                    transport.bind = socket.local_addr().map_err(failure)?;
                    sockets.push(socket);
                }
                Protocol::Tcp | Protocol::Tls | Protocol::Ws | Protocol::Wss => {
                    return Err(ControlError::Unsupported)
                }
            }
        }
        let mut listeners = Vec::with_capacity(sockets.len());
        for (socket, transport) in sockets.into_iter().zip(&config.transports) {
            let mut scoped = config.clone();
            scoped
                .endpoints
                .retain(|endpoint| endpoint.transport == transport.name);
            let mut seed = [0; 32];
            getrandom::fill(&mut seed).map_err(failure)?;
            listeners.push(Listener {
                name: transport.name.clone(),
                socket,
                engine: Engine::new(scoped, seed),
            });
        }
        Ok(Self {
            config,
            controller,
            effects,
            media,
            notices,
            listeners,
            calls: BTreeMap::new(),
            storage: None,
            playbacks: JoinSet::new(),
            recordings: JoinSet::new(),
        })
    }

    pub fn listeners(&self) -> Result<Vec<(String, SocketAddr)>, ControlError> {
        self.listeners
            .iter()
            .map(|listener| {
                Ok((
                    listener.name.clone(),
                    listener.socket.local_addr().map_err(failure)?,
                ))
            })
            .collect()
    }

    async fn transmit(
        &self,
        listener: usize,
        datagrams: Vec<Datagram>,
    ) -> Result<(), ControlError> {
        let socket = &self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
            .socket;
        for packet in datagrams {
            let size = tokio::time::timeout(
                Duration::from_millis(100),
                socket.send_to(&packet.bytes, packet.peer),
            )
            .await
            .map_err(failure)?
            .map_err(failure)?;
            if size != packet.bytes.len() {
                return Err(ControlError::Adapter(
                    "Incomplete signaling datagram".into(),
                ));
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
                        datagram,
                    });
                }
            }
        }
        datagrams.retain(|packet| {
            !self.calls.values().any(|call| {
                call.direction_reply.as_ref().is_some_and(|reply| {
                    reply.datagram.peer == packet.peer && reply.datagram.bytes == packet.bytes
                })
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
                    let sockets = self
                        .calls
                        .get_mut(&key)
                        .and_then(|call| call.pending.take());
                    if let Some(sockets) = sockets {
                        let format = match session.codec {
                            Codec::Pcmu => AudioFormat::Pcmu,
                            Codec::Pcma => AudioFormat::Pcma,
                            Codec::Opus => AudioFormat::Opus,
                        };
                        let direction = peer_direction(session.direction);
                        match self
                            .media
                            .attach(
                                key.clone(),
                                sockets,
                                RtpSession {
                                    remote: session.remote,
                                    format,
                                    payload_type: session.payload_type,
                                    telephone: session
                                        .telephone
                                        .map(|format| {
                                            TelephoneFormat::new(
                                                format.payload_type(),
                                                format.rate(),
                                            )?
                                            .with_events(format.events())
                                        })
                                        .transpose()
                                        .map_err(failure)?,
                                },
                                direction,
                            )
                            .await
                        {
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
        if self.calls.len() >= 64 {
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
        if self.calls.len() >= 64 {
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
        let sockets = SocketPair::bind(
            transport.bind.ip(),
            self.config.rtp.start,
            self.config.rtp.end,
        )
        .map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let local_rtp = SocketAddr::new(
            transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        );
        let dtmf = endpoint.dtmf;
        let output = self.listeners[listener]
            .engine
            .originate(
                Outbound {
                    id: key.id.as_str().into(),
                    generation: key.generation,
                    endpoint: endpoint.name.clone(),
                    caller,
                    local_rtp,
                    timeout_ms: timeout.map(|timeout| u64::from(timeout) * 1000),
                },
                now,
            )
            .map_err(failure)?;
        self.calls.insert(
            key,
            Active {
                listener,
                pending: Some(sockets),
                digits: DigitDelivery::new(dtmf),
                direction_reply: None,
            },
        );
        self.output(listener, output, now).await
    }

    async fn effect(&mut self, effect: Effect, now: u64) -> Result<(), ControlError> {
        match effect {
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
                if self
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
                        let listener = call.listener;
                        if applied {
                            self.transmit(listener, [reply.datagram].into_iter().collect())
                                .await?;
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
                let media = ChannelId::try_from(media)
                    .map(PlaybackMedia::Sound)
                    .map_err(ControlError::from);
                self.start_playback(MediaOwner::Channel(key), token, media)
                    .await?;
            }
            Effect::PlayMedia {
                owner,
                token,
                media,
            } => {
                let media = match media {
                    MediaUri::Sound(name) => PlaybackMedia::Sound(name),
                    MediaUri::Recording(name) => PlaybackMedia::Recording(name),
                };
                self.start_playback(owner, token, Ok(media)).await?;
            }
            Effect::ControlPlayback { token, operation } => self
                .media
                .control_playback(token, operation)
                .map_err(failure)?,
            Effect::Release { key } => {
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
            MediaOwner::Channel(key) => self.calls.contains_key(key),
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
        let sockets = SocketPair::bind(
            transport.bind.ip(),
            self.config.rtp.start,
            self.config.rtp.end,
        )
        .map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let advertised = SocketAddr::new(
            transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        );
        let output = self.listeners[listener]
            .engine
            .answer(key.id.as_str(), key.generation, advertised, now)
            .map_err(failure)?;
        self.calls
            .get_mut(key)
            .ok_or(ControlError::NotFound("Channel not found"))?
            .pending = Some(sockets);
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
                packet = receive(&self.listeners) => {
                    let packet = packet?;
                    let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
                    match self.listeners[packet.listener].engine.receive(&packet.bytes, packet.peer, now) {
                        Ok(output) => self.output(packet.listener, output, now).await?,
                        Err(error) => tracing::debug!(transport = %self.listeners[packet.listener].name, %error, "signaling input refused"),
                    }
                }
                notice = self.notices.recv() => match notice {
                    Some(MediaNotice::DtmfFinished { key, token }) => self.notify(Signal::DtmfFinished { key, token }).await?,
                    Some(MediaNotice::Dtmf { key, tone }) => self.notify(Signal::DtmfReceived { key, digit: tone.digit, duration_ms: tone.duration_ms }).await?,
                    Some(MediaNotice::Connected { key }) => self.notify(Signal::Milestone { key, milestone: Milestone::MediaConnected }).await?,
                    Some(MediaNotice::Stopped { key, result }) => {
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
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; signaling cleanup failed: {cleanup}"
            ))),
        }
    }
}
