use crate::media::{MediaNotice, MediaPort};
use crate::rtp::{RtpSession, SocketPair};
use crate::{CallKey, ControlError, Controller, Effect, IncomingCall, Milestone, Signal};
use rusterisk_api::ChannelId;
use rusterisk_config::{Application, Codec, Config, Protocol};
use rusterisk_media::AudioFormat;
use rusterisk_sip::{Datagram, Engine, Outbound, Output, RejectReason, SipEvent, MAX_MESSAGE};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

struct Listener {
    name: String,
    socket: UdpSocket,
    engine: Engine,
}
struct Active {
    listener: usize,
    pending: Option<SocketPair>,
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
    bridges: BTreeMap<ChannelId, u64>,
    serial: u64,
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
            bridges: BTreeMap::new(),
            serial: 0,
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
        self.transmit(listener, output.datagrams).await?;
        for event in output.events {
            match event {
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
                        match self
                            .media
                            .attach(
                                key.clone(),
                                sockets,
                                RtpSession {
                                    remote: session.remote,
                                    format,
                                    payload_type: session.payload_type,
                                },
                            )
                            .await
                        {
                            Ok(()) => {
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
                        app: String::new(),
                        args: Vec::new(),
                    };
                    if let Err(error) = self.admit(listener, incoming).await {
                        let reason = match error {
                            ControlError::Capacity => RejectReason::Busy,
                            ControlError::NotFound(_) => RejectReason::NoRoute,
                            ControlError::Unsupported => RejectReason::Unsupported,
                            ControlError::Invalid(_)
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

    async fn admit(
        &mut self,
        listener: usize,
        mut incoming: IncomingCall,
    ) -> Result<(), ControlError> {
        if self.calls.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let step = self
            .config
            .dialplan
            .steps
            .iter()
            .find(|step| {
                step.context == incoming.context
                    && step.extension == incoming.extension
                    && step.priority == 1
            })
            .ok_or(ControlError::NotFound("Extension not found"))?;
        match &step.application {
            Application::Stasis {
                application,
                arguments,
            } => {
                incoming.app = application.clone();
                incoming.args = arguments.clone();
            }
            Application::NoOp(_)
            | Application::Set { .. }
            | Application::Answer
            | Application::Dial { .. }
            | Application::Playback(_)
            | Application::Wait(_)
            | Application::Goto { .. }
            | Application::Hangup => return Err(ControlError::Unsupported),
        }
        let key = self.controller.incoming(incoming).await?;
        self.calls.insert(
            key,
            Active {
                listener,
                pending: None,
            },
        );
        Ok(())
    }

    async fn start(
        &mut self,
        key: CallKey,
        endpoint: String,
        caller: String,
        timeout: u32,
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
        let output = self.listeners[listener]
            .engine
            .originate(
                Outbound {
                    id: key.id.as_str().into(),
                    generation: key.generation,
                    endpoint: endpoint.name.clone(),
                    caller,
                    local_rtp,
                    timeout_ms: u64::from(timeout) * 1000,
                },
                now,
            )
            .map_err(failure)?;
        self.calls.insert(
            key,
            Active {
                listener,
                pending: Some(sockets),
            },
        );
        self.output(listener, output, now).await
    }

    async fn effect(&mut self, effect: Effect, now: u64) -> Result<(), ControlError> {
        match effect {
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
                            .ring(key.id.as_str(), now)
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
            Effect::Release { key } => {
                if let Some(call) = self.calls.remove(&key) {
                    let output = self.terminate(call.listener, &key, now).map_err(failure)?;
                    self.transmit(call.listener, output.datagrams).await?;
                }
                self.media.release(key).map_err(failure)?;
            }
            Effect::Bridge { id, channels } => {
                let token = match self.bridges.get(&id) {
                    Some(token) => *token,
                    None if channels.is_empty() => return Ok(()),
                    None => {
                        if self.bridges.len() >= 64 {
                            return Err(ControlError::Capacity);
                        }
                        self.serial = self.serial.checked_add(1).ok_or(ControlError::Capacity)?;
                        self.bridges.insert(id.clone(), self.serial);
                        self.serial
                    }
                };
                if channels.is_empty() {
                    self.bridges.remove(&id);
                }
                self.media.bridge(token, channels).map_err(failure)?;
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

    async fn serve(
        &mut self,
        stop: &mut watch::Receiver<bool>,
        started: Instant,
    ) -> Result<(), ControlError> {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stop.borrow() {
                break;
            }
            let now = u64::try_from(started.elapsed().as_millis()).map_err(failure)?;
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                effect = self.effects.recv() => match effect {
                    Some(effect) => {
                        if let Err(error) = self.effect(effect, now).await { if *stop.borrow() { break; } return Err(error); }
                    }
                    None => break,
                },
                packet = receive(&self.listeners) => {
                    let packet = packet?;
                    match self.listeners[packet.listener].engine.receive(&packet.bytes, packet.peer, now) {
                        Ok(output) => self.output(packet.listener, output, now).await?,
                        Err(error) => tracing::debug!(transport = %self.listeners[packet.listener].name, %error, "signaling input refused"),
                    }
                }
                notice = self.notices.recv() => match notice {
                    Some(MediaNotice::Connected { key }) => self.notify(Signal::Milestone { key, milestone: Milestone::MediaConnected }).await?,
                    Some(MediaNotice::Stopped { key, result }) => {
                        if let Err(error) = result { tracing::warn!(channel = %key.id.as_str(), generation = key.generation, %error, "media stopped"); }
                        self.notify(Signal::Terminated { key, cause: 16 }).await?;
                    }
                    None => { if !*stop.borrow() { return Err(ControlError::Adapter("Media task stopped".into())); } break; }
                },
                _instant = tick.tick() => {
                    for listener in 0..self.listeners.len() {
                        let output = self.listeners[listener].engine.tick(now).map_err(failure)?;
                        self.output(listener, output, now).await?;
                    }
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
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
                "{primary}; signaling cleanup failed: {cleanup}"
            ))),
        }
    }
}
