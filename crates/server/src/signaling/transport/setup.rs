use super::{
    connection, failure, finished, ConnectionKey, ConnectionState, ControlError, ListenerSocket,
    Network, Port, Reserved, StreamPacket,
};
use crate::runner::CallLease;
use crate::signaling::resolver::ResolvedTarget;
use std::{future::Future, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpSocket, TcpStream},
    sync::{mpsc, oneshot, watch},
};
use tokio_rustls::rustls::pki_types::ServerName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SetupKey {
    pub(crate) listener: usize,
    generation: u64,
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

pub(super) struct Preparation {
    stop: watch::Sender<bool>,
    phase: Phase,
    identity: Option<String>,
}

pub(crate) struct Prepared {
    pub(crate) key: SetupKey,
    peer: SocketAddr,
    reply: oneshot::Sender<Reserved>,
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
    lease: CallLease,
    setup_sender: mpsc::Sender<SetupEvent>,
    sender: mpsc::Sender<StreamPacket>,
    stop: watch::Receiver<bool>,
    deadline: tokio::time::Instant,
}

impl Network {
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
    ) -> Result<SetupKey, ControlError> {
        if self.ports.len() + self.preparations.len() >= 64 || self.jobs.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        if !matches!(
            self.listeners.get(listener),
            Some(
                ListenerSocket::Datagram(_)
                    | ListenerSocket::Stream(_)
                    | ListenerSocket::Encrypted { .. }
            )
        ) {
            return Err(ControlError::Unsupported);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        let key = SetupKey {
            listener,
            generation: self.generation,
        };
        let (stop, _stopped) = watch::channel(false);
        self.preparations.insert(
            key,
            Preparation {
                stop,
                phase: Phase::Resolving,
                identity: None,
            },
        );
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
            .is_some_and(|preparation| matches!(preparation.phase, Phase::Resolving))
        {
            self.preparations.remove(&key);
        }
    }

    pub(in crate::signaling) fn connect_prepared(
        &mut self,
        key: SetupKey,
        resolved: ResolvedTarget,
        lease: CallLease,
    ) -> Result<(), ControlError> {
        if self.jobs.len() >= 64 {
            return Err(ControlError::Capacity);
        }
        if lease.is_released() {
            return Err(ControlError::Conflict("Call ended during signaling setup"));
        }
        let encryption = match self
            .listeners
            .get(key.listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
        {
            ListenerSocket::Stream(_) => None,
            ListenerSocket::Encrypted { client, .. } => Some(client.clone()),
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
        preparation.phase = Phase::Connecting;
        preparation.identity = encryption.as_ref().map(|_| resolved.identity().to_owned());
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
        Ok(())
    }

    pub(in crate::signaling) fn admit_prepared(
        &mut self,
        prepared: Prepared,
    ) -> Result<ConnectionKey, ControlError> {
        let preparation = self
            .preparations
            .get(&prepared.key)
            .ok_or(ControlError::Conflict("Signaling setup ended"))?;
        if *preparation.stop.borrow() {
            return Err(ControlError::Conflict("Signaling setup cancelled"));
        }
        if let Some(port) = self.ports.get(&(prepared.key.listener, prepared.peer)) {
            if !matches!(port.state, ConnectionState::Open)
                || (preparation.identity.is_some() && preparation.identity != port.identity)
            {
                return Err(ControlError::Conflict(
                    "Signaling peer belongs to a different flow identity",
                ));
            }
            return Ok(port.key);
        }
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
            },
        );
        if prepared
            .reply
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
        Ok(key)
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
    let connected = tokio::select! {
        _changed = job.stop.changed() => Err(ControlError::Conflict("Signaling setup cancelled")),
        _released = job.lease.released() => Err(ControlError::Conflict("Call ended during signaling setup")),
        result = tokio::time::timeout_at(job.deadline, open(local, &resolved, connector)) => result.map_err(failure).and_then(|result| result),
    };
    match connected {
        Ok((peer, stream)) => admission(stream, peer, job).await,
        Err(error) => setup_finished(&job.setup_sender, job.key, Err(error)).await,
    }
}

async fn open<S, F, C>(
    local: SocketAddr,
    resolved: &ResolvedTarget,
    mut connector: F,
) -> Result<(SocketAddr, S), ControlError>
where
    F: FnMut(TcpStream, ServerName<'static>) -> C,
    C: Future<Output = Result<S, ControlError>>,
{
    let mut last = ControlError::NotFound("No usable signaling address");
    for peer in resolved.peers() {
        if peer.is_ipv4() != local.is_ipv4() {
            continue;
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
        let name = ServerName::try_from(resolved.identity().to_owned()).map_err(failure)?;
        match connector(stream, name).await {
            Ok(stream) => return Ok((peer, stream)),
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
    let (reply, admitted) = oneshot::channel();
    let waiting = async {
        job.setup_sender
            .send(SetupEvent::Ready(Prepared {
                key: job.key,
                peer,
                reply,
            }))
            .await
            .map_err(failure)?;
        admitted
            .await
            .map_err(|_| ControlError::Conflict("Signaling setup superseded"))
    };
    let admitted = tokio::select! {
        _changed = job.stop.changed() => Err(ControlError::Conflict("Signaling setup cancelled")),
        _released = job.lease.released() => Err(ControlError::Conflict("Call ended during signaling setup")),
        admitted = tokio::time::timeout_at(job.deadline, waiting) => admitted.map_err(failure).and_then(|result| result),
    };
    match admitted {
        Ok(Reserved { key, packets, stop }) => {
            if job.lease.is_released() {
                return finished(
                    &job.sender,
                    key,
                    Err(ControlError::Conflict(
                        "Call ended before signaling admission",
                    )),
                    &stop,
                )
                .await;
            }
            connection(stream, key, packets, job.sender, stop).await
        }
        Err(error) => setup_finished(&job.setup_sender, job.key, Err(error)).await,
    }
}
