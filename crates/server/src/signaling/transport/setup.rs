use super::{
    connection, failure, finished, ConnectionKey, ConnectionState, ControlError, ListenerSocket,
    Network, Port, Reserved, StreamPacket, StreamRole,
};
use crate::signaling::{registrations::ConfigurationLease, resolver::ResolvedTarget};
use crate::{encryption::TlsClientContext, runner::CallLease};
use candidate::{CandidateDecision, PreparedReply};
use std::{future::Future, net::SocketAddr, num::NonZeroU64, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpSocket, TcpStream},
    sync::{mpsc, oneshot, watch},
};
use tokio_rustls::rustls::pki_types::ServerName;
mod candidate;
#[cfg(test)]
mod profile_tests;
#[cfg(test)]
mod reuse_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SetupKey {
    pub(crate) listener: usize,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::signaling) enum ConnectionSetup {
    Reused(ConnectionKey),
    Connecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::signaling) enum ConnectionAdmission {
    Continue,
    Ready(ConnectionKey),
}

#[derive(Clone, Copy)]
pub(crate) enum RegisteredRoute {
    Datagram { listener: usize, peer: SocketAddr },
    Stream(ConnectionKey),
}

impl RegisteredRoute {
    pub(crate) const fn peer(self) -> SocketAddr {
        match self {
            Self::Datagram { peer, .. } => peer,
            Self::Stream(key) => key.peer,
        }
    }
}

pub(super) enum Phase {
    Resolving,
    Connecting,
}

#[derive(Clone)]
pub(super) struct StartingTlsProfile {
    configuration: NonZeroU64,
    context: TlsClientContext,
}

pub(super) struct Preparation {
    stop: watch::Sender<bool>,
    phase: Phase,
    identity: Option<String>,
    tls_profile: Option<StartingTlsProfile>,
}

impl Preparation {
    fn matches_flow(&self, port: &Port) -> bool {
        let identities = match (&self.identity, &port.identity) {
            (None, None) => true,
            (Some(expected), Some(actual)) => expected
                .strip_suffix('.')
                .unwrap_or(expected)
                .eq_ignore_ascii_case(actual.strip_suffix('.').unwrap_or(actual)),
            (Some(_), None) | (None, Some(_)) => false,
        };
        let profiles = match (&self.tls_profile, &port.tls_profile) {
            (None, None) => true,
            (Some(expected), Some(actual)) => {
                expected.configuration == actual.configuration
                    && expected.context.same_profile(&actual.context)
            }
            (Some(_), None) | (None, Some(_)) => false,
        };
        matches!(port.state, ConnectionState::Open)
            && !port.packets.is_closed()
            && !*port.stop.borrow()
            && identities
            && profiles
    }
}

pub(crate) struct Prepared {
    pub(crate) key: SetupKey,
    peer: SocketAddr,
    reply: PreparedReply,
}

pub(super) enum SetupEvent {
    Ready(Prepared),
    Finished {
        key: SetupKey,
        result: Result<(), ControlError>,
    },
}

struct SetupJob {
    key: SetupKey,
    lease: SetupLifetime,
    setup_sender: mpsc::Sender<SetupEvent>,
    sender: mpsc::Sender<StreamPacket>,
    stop: watch::Receiver<bool>,
    deadline: tokio::time::Instant,
}

enum Opened<S> {
    Stream { peer: SocketAddr, stream: S },
    Reused,
}

pub(in crate::signaling) enum SetupLifetime {
    Call(CallLease),
    Configuration(ConfigurationLease),
}

impl SetupLifetime {
    fn is_released(&self) -> bool {
        match self {
            Self::Call(lease) => lease.is_released(),
            Self::Configuration(lease) => lease.is_released(),
        }
    }

    async fn released(&self) {
        match self {
            Self::Call(lease) => lease.released().await,
            Self::Configuration(lease) => lease.released().await,
        }
    }
}

fn reusable_candidate(
    mut peers: impl Iterator<Item = SocketAddr>,
    local: SocketAddr,
    current: impl FnOnce(SocketAddr) -> Option<ConnectionKey>,
) -> Result<Option<ConnectionKey>, ControlError> {
    let peer = peers
        .find(|peer| peer.is_ipv4() == local.is_ipv4())
        .ok_or(ControlError::NotFound("No usable signaling address"))?;
    Ok(current(peer))
}

impl Network {
    pub(super) fn connection_slots(&self) -> usize {
        self.ports.len()
            + self
                .preparations
                .values()
                .filter(|preparation| matches!(preparation.phase, Phase::Connecting))
                .count()
    }

    pub(super) fn close_preparations(&self) {
        for preparation in self.preparations.values() {
            preparation.stop.send_replace(true);
        }
    }

    pub(in crate::signaling) fn registered_route(
        &self,
        listener: usize,
        peer: SocketAddr,
    ) -> Result<RegisteredRoute, ControlError> {
        match self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
        {
            ListenerSocket::Datagram(_) => Ok(RegisteredRoute::Datagram { listener, peer }),
            ListenerSocket::Stream(_)
            | ListenerSocket::Encrypted { .. }
            | ListenerSocket::WebSocket { .. } => {
                let port = self
                    .ports
                    .get(&(listener, peer))
                    .filter(|port| matches!(port.state, ConnectionState::Open))
                    .ok_or(ControlError::Conflict("Registered signaling flow closed"))?;
                Ok(RegisteredRoute::Stream(port.key))
            }
        }
    }

    pub(in crate::signaling) fn route_is_current(&self, route: RegisteredRoute) -> bool {
        match route {
            RegisteredRoute::Datagram { listener, .. } => matches!(
                self.listeners.get(listener),
                Some(ListenerSocket::Datagram(_))
            ),
            RegisteredRoute::Stream(key) => self
                .ports
                .get(&(key.listener, key.peer))
                .is_some_and(|port| port.key == key && matches!(port.state, ConnectionState::Open)),
        }
    }

    pub(in crate::signaling) fn reserve_setup(
        &mut self,
        listener: usize,
        configuration: NonZeroU64,
    ) -> Result<SetupKey, ControlError> {
        let tls_profile = match self.listeners.get(listener) {
            Some(ListenerSocket::Datagram(_) | ListenerSocket::Stream(_)) => None,
            Some(ListenerSocket::Encrypted { client, .. }) => Some(StartingTlsProfile {
                configuration,
                context: client.clone(),
            }),
            Some(ListenerSocket::WebSocket { .. }) | None => return Err(ControlError::Unsupported),
        };
        let key = self.reserve_slot(listener)?;
        let (stop, _stopped) = watch::channel(false);
        self.preparations.insert(
            key,
            Preparation {
                stop,
                phase: Phase::Resolving,
                identity: None,
                tls_profile,
            },
        );
        Ok(key)
    }

    pub(in crate::signaling) fn reserve_registered_setup(
        &mut self,
        listener: usize,
    ) -> Result<SetupKey, ControlError> {
        self.reserve_slot(listener)
    }

    fn reserve_slot(&mut self, listener: usize) -> Result<SetupKey, ControlError> {
        if self.setups.len() >= 16 {
            return Err(ControlError::Capacity);
        }
        if self.listeners.get(listener).is_none() {
            return Err(ControlError::Invalid("Unknown signaling listener"));
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        let key = SetupKey {
            listener,
            generation: self.generation,
        };
        self.setups.insert(key);
        Ok(key)
    }

    pub(in crate::signaling) fn cancel_setup(&mut self, key: SetupKey) {
        if let Some(preparation) = self.preparations.get(&key) {
            preparation.stop.send_replace(true);
        }
    }

    pub(in crate::signaling) fn finish_resolution(&mut self, key: SetupKey) {
        if self
            .preparations
            .get(&key)
            .is_none_or(|preparation| matches!(preparation.phase, Phase::Resolving))
        {
            self.preparations.remove(&key);
            self.setups.remove(&key);
        }
    }

    pub(in crate::signaling) fn connect_prepared(
        &mut self,
        key: SetupKey,
        resolved: ResolvedTarget,
        lease: SetupLifetime,
    ) -> Result<ConnectionSetup, ControlError> {
        if lease.is_released() {
            return Err(ControlError::Conflict("Signaling setup owner ended"));
        }
        let encryption = match self
            .listeners
            .get(key.listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
        {
            ListenerSocket::Stream(_) => None,
            ListenerSocket::Encrypted { .. } => Some(
                self.preparations
                    .get(&key)
                    .and_then(|preparation| preparation.tls_profile.as_ref())
                    .ok_or(ControlError::Invalid("TLS starting profile missing"))?
                    .context
                    .clone(),
            ),
            ListenerSocket::Datagram(_) | ListenerSocket::WebSocket { .. } => {
                return Err(ControlError::Unsupported)
            }
        };
        let local = SocketAddr::new(self.local_addr(key.listener)?.ip(), 0);
        let preparation = self
            .preparations
            .get_mut(&key)
            .ok_or(ControlError::Conflict("Signaling setup ended"))?;
        if *preparation.stop.borrow() || !matches!(preparation.phase, Phase::Resolving) {
            return Err(ControlError::Conflict("Signaling setup cancelled"));
        }
        preparation.identity = encryption.as_ref().map(|_| resolved.identity().to_owned());
        let reused = reusable_candidate(resolved.peers(), local, |peer| {
            self.ports
                .get(&(key.listener, peer))
                .filter(|port| preparation.matches_flow(port))
                .map(|port| port.key)
        })?;
        if let Some(connection) = reused {
            self.finish_resolution(key);
            return Ok(ConnectionSetup::Reused(connection));
        }
        if self.connection_slots() >= 64 || self.jobs.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        let preparation = self
            .preparations
            .get_mut(&key)
            .ok_or(ControlError::Conflict("Signaling setup ended"))?;
        preparation.phase = Phase::Connecting;
        let job = SetupJob {
            key,
            lease,
            setup_sender: self.preparation_sender.clone(),
            sender: self.sender.clone(),
            stop: preparation.stop.subscribe(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(8),
        };
        match encryption {
            None => self
                .jobs
                .spawn(prepare(local, resolved, job, |stream, _name| {
                    std::future::ready(Ok(stream))
                })),
            Some(context) => self
                .jobs
                .spawn(prepare(local, resolved, job, move |stream, name| {
                    let context = context.clone();
                    async move { context.connect(stream, name).await }
                })),
        };
        Ok(ConnectionSetup::Connecting)
    }

    pub(in crate::signaling) fn admit_prepared(
        &mut self,
        prepared: Prepared,
    ) -> Result<ConnectionAdmission, ControlError> {
        let preparation = self
            .preparations
            .get(&prepared.key)
            .ok_or(ControlError::Conflict("Signaling setup ended"))?;
        if *preparation.stop.borrow() || prepared.reply.is_closed() {
            return Err(ControlError::Conflict("Signaling setup cancelled"));
        }
        if let Some(port) = self.ports.get(&(prepared.key.listener, prepared.peer)) {
            if !preparation.matches_flow(port) {
                return Err(ControlError::Conflict(
                    "Signaling peer belongs to a different flow identity",
                ));
            }
            let key = port.key;
            match prepared.reply {
                PreparedReply::Candidate(reply) => reply
                    .send(CandidateDecision::Reuse)
                    .map_err(|_| ControlError::Conflict("Signaling candidate owner closed"))?,
                PreparedReply::Connected(reply) => drop(reply),
            }
            return Ok(ConnectionAdmission::Ready(key));
        }
        let reply = match prepared.reply {
            PreparedReply::Candidate(reply) => {
                reply
                    .send(CandidateDecision::Connect)
                    .map_err(|_| ControlError::Conflict("Signaling candidate owner closed"))?;
                return Ok(ConnectionAdmission::Continue);
            }
            PreparedReply::Connected(reply) => reply,
        };
        let preparation = self
            .preparations
            .remove(&prepared.key)
            .ok_or(ControlError::Capacity)?;
        let key = ConnectionKey {
            listener: prepared.key.listener,
            peer: prepared.peer,
            generation: prepared.key.generation,
        };
        let (packets, receiver) = mpsc::channel(8);
        let stopped = preparation.stop.subscribe();
        self.ports.insert(
            (key.listener, key.peer),
            Port {
                key,
                packets,
                stop: preparation.stop,
                state: ConnectionState::Open,
                identity: preparation.identity,
                tls_profile: preparation.tls_profile,
            },
        );
        if reply
            .send(Reserved {
                key,
                packets: receiver,
                stop: stopped,
            })
            .is_err()
        {
            self.ports.remove(&(key.listener, key.peer));
            return Err(ControlError::Conflict("Signaling setup owner closed"));
        }
        self.setups.remove(&prepared.key);
        Ok(ConnectionAdmission::Ready(key))
    }

    pub(super) fn prepared_event(&mut self, event: SetupEvent) -> Option<super::Input> {
        match event {
            SetupEvent::Ready(prepared) => {
                if self.preparations.contains_key(&prepared.key) {
                    Some(super::Input::Prepared(prepared))
                } else {
                    None
                }
            }
            SetupEvent::Finished { key, result } => {
                self.setups.remove(&key);
                if self.preparations.remove(&key).is_some() {
                    return Some(super::Input::SetupFinished { key, result });
                }
                let flow = self
                    .ports
                    .values()
                    .find(|port| {
                        port.key.listener == key.listener && port.key.generation == key.generation
                    })
                    .map(|port| port.key);
                flow.map(|flow| {
                    self.ports.remove(&(flow.listener, flow.peer));
                    super::Input::Closed { key: flow, result }
                })
            }
        }
    }
}

async fn prepare<S, F, C>(
    local: SocketAddr,
    resolved: ResolvedTarget,
    mut job: SetupJob,
    connector: F,
) -> Result<(), ControlError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(TcpStream, ServerName<'static>) -> C,
    C: Future<Output = Result<S, ControlError>>,
{
    let cancelled = *job.stop.borrow() || job.lease.is_released();
    if cancelled {
        return setup_finished(
            &job.setup_sender,
            job.key,
            Err(ControlError::Conflict("Signaling setup owner ended")),
        )
        .await;
    }
    let stopped = job.stop.clone();
    let connected = tokio::select! {
        biased;
        _changed = job.stop.changed() => Err(ControlError::Conflict("Signaling setup cancelled")),
        _released = job.lease.released() => Err(ControlError::Conflict("Signaling setup owner ended")),
        result = tokio::time::timeout_at(job.deadline, open(local, &resolved, connector, &job.lease, stopped, &job.setup_sender, job.key)) => result.map_err(failure).and_then(|result| result),
    };
    match connected {
        Ok(Opened::Stream { peer, stream }) => admission(stream, peer, job).await,
        Ok(Opened::Reused) => setup_finished(&job.setup_sender, job.key, Ok(())).await,
        Err(error) => setup_finished(&job.setup_sender, job.key, Err(error)).await,
    }
}

async fn open<S, F, C>(
    local: SocketAddr,
    resolved: &ResolvedTarget,
    mut connector: F,
    lease: &SetupLifetime,
    stopped: watch::Receiver<bool>,
    sender: &mpsc::Sender<SetupEvent>,
    key: SetupKey,
) -> Result<Opened<S>, ControlError>
where
    F: FnMut(TcpStream, ServerName<'static>) -> C,
    C: Future<Output = Result<S, ControlError>>,
{
    let mut last = ControlError::NotFound("No usable signaling address");
    for (index, peer) in resolved
        .peers()
        .filter(|peer| peer.is_ipv4() == local.is_ipv4())
        .enumerate()
    {
        let cancelled = *stopped.borrow() || lease.is_released();
        if cancelled {
            return Err(ControlError::Conflict("Signaling setup owner ended"));
        }
        if index != 0 {
            match candidate::inspect(sender, key, peer).await? {
                CandidateDecision::Reuse => return Ok(Opened::Reused),
                CandidateDecision::Connect => {}
            }
            if *stopped.borrow() || lease.is_released() {
                return Err(ControlError::Conflict("Signaling setup owner ended"));
            }
        }
        let socket = if peer.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        }
        .map_err(failure)?;
        socket.bind(local).map_err(failure)?;
        let stream = match tokio::time::timeout(Duration::from_secs(2), socket.connect(peer)).await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                last = failure(error);
                continue;
            }
            Err(error) => {
                last = failure(error);
                continue;
            }
        };
        stream.set_nodelay(true).map_err(failure)?;
        let cancelled = *stopped.borrow() || lease.is_released();
        if cancelled {
            return Err(ControlError::Conflict("Signaling setup owner ended"));
        }
        let name = ServerName::try_from(resolved.identity().to_owned()).map_err(failure)?;
        match connector(stream, name).await {
            Ok(stream) => return Ok(Opened::Stream { peer, stream }),
            Err(error) => last = error,
        }
    }
    Err(last)
}

async fn setup_finished(
    sender: &mpsc::Sender<SetupEvent>,
    key: SetupKey,
    result: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match sender.send(SetupEvent::Finished { key, result }).await {
        Ok(()) => Ok(()),
        Err(_) if sender.is_closed() => Ok(()),
        Err(error) => Err(failure(error)),
    }
}

async fn admission<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    peer: SocketAddr,
    mut job: SetupJob,
) -> Result<(), ControlError> {
    let cancelled = *job.stop.borrow() || job.lease.is_released();
    if cancelled {
        return setup_finished(
            &job.setup_sender,
            job.key,
            Err(ControlError::Conflict("Signaling setup owner ended")),
        )
        .await;
    }
    let (reply, admitted) = oneshot::channel();
    let waiting = async {
        job.setup_sender
            .send(SetupEvent::Ready(Prepared {
                key: job.key,
                peer,
                reply: PreparedReply::Connected(reply),
            }))
            .await
            .map_err(failure)?;
        admitted
            .await
            .map_err(|_| ControlError::Conflict("Signaling setup superseded"))
    };
    let admitted = tokio::select! {
        biased;
        _changed = job.stop.changed() => Err(ControlError::Conflict("Signaling setup cancelled")),
        _released = job.lease.released() => Err(ControlError::Conflict("Signaling setup owner ended")),
        admitted = tokio::time::timeout_at(job.deadline, waiting) => admitted.map_err(failure).and_then(|result| result),
    };
    match admitted {
        Ok(Reserved { key, packets, stop }) => {
            if job.lease.is_released() {
                return finished(
                    &job.sender,
                    key,
                    Err(ControlError::Conflict(
                        "Signaling setup owner ended before admission",
                    )),
                    &stop,
                )
                .await;
            }
            connection(
                stream,
                key,
                packets,
                job.sender,
                stop,
                StreamRole::Initiated,
            )
            .await
        }
        Err(error) => setup_finished(&job.setup_sender, job.key, Err(error)).await,
    }
}
