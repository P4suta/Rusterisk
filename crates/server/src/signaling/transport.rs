use super::sip_websocket::{Scheme, SipWebSocketPort, UpgradeRequest};
use super::{failure, Received};
use crate::{
    encryption::{TlsClientContext, TlsContext},
    ControlError,
};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Datagram, SipStreamDecoder, StreamItem, MAX_MESSAGE};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinSet,
};
#[cfg(test)]
mod refer_tests;
mod setup;
#[cfg(test)]
mod tests;
mod websocket;
pub(super) use setup::{
    ConnectionAdmission, ConnectionSetup, Prepared, RegisteredRoute, SetupKey, SetupLifetime,
};

enum ListenerSocket {
    Datagram(UdpSocket),
    Stream(TcpListener),
    Encrypted {
        listener: TcpListener,
        context: TlsContext,
        client: TlsClientContext,
    },
    WebSocket {
        address: SocketAddr,
        scheme: Scheme,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ConnectionKey {
    pub listener: usize,
    pub peer: SocketAddr,
    generation: u64,
}

#[derive(Clone, Copy)]
enum Route {
    Datagram { listener: usize, peer: SocketAddr },
    Stream(ConnectionKey),
    Closed { listener: usize, peer: SocketAddr },
}

#[derive(Clone, Copy)]
pub(super) struct DeliveryRoute(Route);

impl DeliveryRoute {
    pub(super) fn matches_connection(self, connection: ConnectionKey) -> bool {
        matches!(self.0, Route::Stream(key) if key == connection)
    }

    pub(super) const fn listener(self) -> usize {
        match self.0 {
            Route::Datagram { listener, .. } | Route::Closed { listener, .. } => listener,
            Route::Stream(key) => key.listener,
        }
    }

    pub(super) const fn peer(self) -> SocketAddr {
        match self.0 {
            Route::Datagram { peer, .. } | Route::Closed { peer, .. } => peer,
            Route::Stream(key) => key.peer,
        }
    }
}

pub(super) struct DeferredTransmission {
    route: DeliveryRoute,
    packet: Datagram,
}

impl DeferredTransmission {
    pub(super) fn matches(&self, listener: usize, packet: &Datagram) -> bool {
        self.route.listener() == listener
            && self.packet.peer == packet.peer
            && self.packet.bytes == packet.bytes
    }
}

enum ConnectionState {
    Open,
    Closing(ControlError),
}

struct Port {
    key: ConnectionKey,
    packets: mpsc::Sender<Vec<u8>>,
    stop: watch::Sender<bool>,
    state: ConnectionState,
    identity: Option<String>,
    tls_profile: Option<setup::StartingTlsProfile>,
}

struct StreamPacket {
    key: ConnectionKey,
    item: StreamEvent,
}

enum StreamEvent {
    Message(Vec<u8>),
    Finished(Result<(), ControlError>),
}

enum FrameDeadline {
    FirstMessage(tokio::time::Instant),
    Message(tokio::time::Instant),
    Idle,
}

enum StreamRole {
    Accepted,
    Initiated,
}

struct Reserved {
    key: ConnectionKey,
    packets: mpsc::Receiver<Vec<u8>>,
    stop: watch::Receiver<bool>,
}

enum ListenerInput {
    Datagram(Received),
    Accepted {
        listener: usize,
        peer: SocketAddr,
        stream: TcpStream,
    },
}

pub(super) enum Input {
    Packet(Received),
    Prepared(Prepared),
    SetupFinished {
        key: SetupKey,
        result: Result<(), ControlError>,
    },
    Closed {
        key: ConnectionKey,
        result: Result<(), ControlError>,
    },
}

pub(super) struct Network {
    listeners: Vec<ListenerSocket>,
    ports: BTreeMap<(usize, SocketAddr), Port>,
    jobs: JoinSet<Result<(), ControlError>>,
    sender: mpsc::Sender<StreamPacket>,
    received: mpsc::Receiver<StreamPacket>,
    generation: u64,
    upgrades: mpsc::Receiver<UpgradeRequest>,
    upgrade_requests: mpsc::Sender<UpgradeRequest>,
    preparations: BTreeMap<SetupKey, setup::Preparation>,
    setups: BTreeSet<SetupKey>,
    preparation_sender: mpsc::Sender<setup::SetupEvent>,
    prepared: mpsc::Receiver<setup::SetupEvent>,
}

async fn listen(listeners: &[ListenerSocket]) -> Result<ListenerInput, ControlError> {
    std::future::poll_fn(|context| {
        let mut bytes = [0; MAX_MESSAGE + 1];
        for (index, listener) in listeners.iter().enumerate() {
            let result = match listener {
                ListenerSocket::WebSocket { .. } => continue,
                ListenerSocket::Datagram(socket) => {
                    let mut buffer = ReadBuf::new(&mut bytes);
                    match socket.poll_recv_from(context, &mut buffer) {
                        Poll::Ready(Ok(peer)) => {
                            return Poll::Ready(Ok(ListenerInput::Datagram(Received {
                                listener: index,
                                peer,
                                bytes: buffer.filled().to_vec(),
                            })));
                        }
                        Poll::Ready(Err(error)) => return Poll::Ready(Err(failure(error))),
                        Poll::Pending => continue,
                    }
                }
                ListenerSocket::Stream(listener) | ListenerSocket::Encrypted { listener, .. } => {
                    listener.poll_accept(context)
                }
            };
            match result {
                Poll::Ready(Ok((stream, peer))) => {
                    return Poll::Ready(Ok(ListenerInput::Accepted {
                        listener: index,
                        peer,
                        stream,
                    }));
                }
                Poll::Ready(Err(error)) => return Poll::Ready(Err(failure(error))),
                Poll::Pending => {}
            }
        }
        Poll::Pending
    })
    .await
}

impl Network {
    pub(super) fn current(&self, route: DeliveryRoute) -> bool {
        match route.0 {
            Route::Datagram { listener, .. } => matches!(
                self.listeners.get(listener),
                Some(ListenerSocket::Datagram(_))
            ),
            Route::Stream(key) => self
                .ports
                .get(&(key.listener, key.peer))
                .is_some_and(|port| port.key == key && matches!(port.state, ConnectionState::Open)),
            Route::Closed { .. } => false,
        }
    }

    pub(super) fn capture(
        &self,
        listener: usize,
        peer: SocketAddr,
    ) -> Result<DeliveryRoute, ControlError> {
        let route = match self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
        {
            ListenerSocket::Datagram(_) => Route::Datagram { listener, peer },
            ListenerSocket::Stream(_)
            | ListenerSocket::Encrypted { .. }
            | ListenerSocket::WebSocket { .. } => match self.ports.get(&(listener, peer)) {
                Some(port) if matches!(port.state, ConnectionState::Open) => {
                    Route::Stream(port.key)
                }
                Some(_) | None => Route::Closed { listener, peer },
            },
        };
        Ok(DeliveryRoute(route))
    }

    pub(super) fn defer(
        &self,
        listener: usize,
        packet: Datagram,
    ) -> Result<DeferredTransmission, ControlError> {
        if packet.bytes.len() > MAX_MESSAGE {
            return Err(ControlError::Capacity);
        }
        Ok(DeferredTransmission {
            route: self.capture(listener, packet.peer)?,
            packet,
        })
    }

    pub(super) async fn send_deferred(
        &mut self,
        deferred: DeferredTransmission,
    ) -> Result<bool, ControlError> {
        self.send_on(deferred.route, deferred.packet).await
    }

    pub(super) async fn send_on(
        &mut self,
        route: DeliveryRoute,
        packet: Datagram,
    ) -> Result<bool, ControlError> {
        if packet.bytes.len() > MAX_MESSAGE {
            return Err(ControlError::Capacity);
        }
        if packet.peer != route.peer() {
            return Err(ControlError::Invalid("Delivery peer changed"));
        }
        if !self.current(route) {
            return Ok(false);
        }
        self.send(route.listener(), packet).await?;
        Ok(true)
    }

    pub(super) async fn bind(config: &mut Config) -> Result<Self, ControlError> {
        let mut listeners = Vec::with_capacity(config.transports.len());
        for transport in &mut config.transports {
            let listener = match transport.protocol {
                Protocol::Udp => ListenerSocket::Datagram(
                    UdpSocket::bind(transport.bind).await.map_err(failure)?,
                ),
                Protocol::Tcp => ListenerSocket::Stream(
                    TcpListener::bind(transport.bind).await.map_err(failure)?,
                ),
                Protocol::Tls => ListenerSocket::Encrypted {
                    listener: TcpListener::bind(transport.bind).await.map_err(failure)?,
                    context: TlsContext::load(
                        transport
                            .tls
                            .as_ref()
                            .ok_or(ControlError::Invalid("Encryption configuration missing"))?,
                    )
                    .await?,
                    client: TlsClientContext::load_sip(transport.ca_list_file.as_deref())?,
                },
                Protocol::Ws => ListenerSocket::WebSocket {
                    address: config.http.bind,
                    scheme: Scheme::Ws,
                },
                Protocol::Wss => ListenerSocket::WebSocket {
                    address: config.http.bind,
                    scheme: Scheme::Wss,
                },
            };
            transport.bind = match &listener {
                ListenerSocket::WebSocket { address, .. } => Ok(*address),
                ListenerSocket::Datagram(socket) => socket.local_addr(),
                ListenerSocket::Stream(socket)
                | ListenerSocket::Encrypted {
                    listener: socket, ..
                } => socket.local_addr(),
            }
            .map_err(failure)?;
            listeners.push(listener);
        }
        let (sender, received) = mpsc::channel(128);
        let (upgrade_requests, upgrades) = mpsc::channel(64);
        let (preparation_sender, prepared) = mpsc::channel(64);
        Ok(Self {
            listeners,
            ports: BTreeMap::new(),
            jobs: JoinSet::new(),
            sender,
            received,
            generation: 0,
            upgrades,
            upgrade_requests,
            preparations: BTreeMap::new(),
            setups: BTreeSet::new(),
            preparation_sender,
            prepared,
        })
    }

    pub(super) fn websocket_port(&self, origins: Vec<String>) -> SipWebSocketPort {
        SipWebSocketPort::new(self.upgrade_requests.clone(), origins)
    }

    pub(super) fn local_addr(&self, listener: usize) -> Result<SocketAddr, ControlError> {
        match self
            .listeners
            .get(listener)
            .ok_or(ControlError::Invalid("Unknown listener"))?
        {
            ListenerSocket::WebSocket { address, .. } => return Ok(*address),
            ListenerSocket::Datagram(socket) => socket.local_addr(),
            ListenerSocket::Stream(socket)
            | ListenerSocket::Encrypted {
                listener: socket, ..
            } => socket.local_addr(),
        }
        .map_err(failure)
    }

    fn reserve(&mut self, listener: usize, peer: SocketAddr) -> Result<Reserved, ControlError> {
        if self.connection_slots() >= 64
            || self.jobs.len() >= 64
            || self.ports.contains_key(&(listener, peer))
        {
            return Err(ControlError::Capacity);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        let key = ConnectionKey {
            listener,
            peer,
            generation: self.generation,
        };
        let (packets, input) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        self.ports.insert(
            (listener, peer),
            Port {
                key,
                packets,
                stop,
                state: ConnectionState::Open,
                identity: None,
                tls_profile: None,
            },
        );
        Ok(Reserved {
            key,
            packets: input,
            stop: stopped,
        })
    }

    fn upgrade(
        &mut self,
        scheme: Scheme,
        peer: SocketAddr,
    ) -> Result<tokio::sync::oneshot::Sender<axum::extract::ws::WebSocket>, ControlError> {
        let listener = self.listeners.iter().position(|listener| matches!(listener, ListenerSocket::WebSocket { scheme: configured, .. } if *configured == scheme)).ok_or(ControlError::Unsupported)?;
        let Reserved { key, packets, stop } = self.reserve(listener, peer)?;
        let (socket, received) = tokio::sync::oneshot::channel();
        let sender = self.sender.clone();
        self.jobs.spawn(async move {
            websocket::connection(received, key, packets, sender, stop).await
        });
        Ok(socket)
    }

    fn accepted(
        &mut self,
        listener: usize,
        peer: SocketAddr,
        stream: TcpStream,
    ) -> Result<(), ControlError> {
        let encryption = match &self.listeners[listener] {
            ListenerSocket::Stream(_) => None,
            ListenerSocket::Encrypted { context, .. } => Some(context.clone()),
            ListenerSocket::Datagram(_) | ListenerSocket::WebSocket { .. } => {
                return Err(ControlError::Invalid(
                    "Stream accepted on datagram listener",
                ));
            }
        };
        stream.set_nodelay(true).map_err(failure)?;
        let Reserved {
            key,
            packets,
            mut stop,
        } = self.reserve(listener, peer)?;
        let sender = self.sender.clone();
        self.jobs.spawn(async move {
            match encryption {
                None => connection(stream, key, packets, sender, stop, StreamRole::Accepted).await,
                Some(context) => {
                    tokio::select! {
                        changed = stop.changed() => match changed { Ok(()) | Err(_) => finished(&sender, key, Ok(()), &stop).await },
                        accepted = context.accept(stream) => match accepted { Ok(stream) => connection(stream, key, packets, sender, stop, StreamRole::Accepted).await, Err(error) => finished(&sender, key, Err(error), &stop).await },
                    }
                }
            }
        });
        Ok(())
    }

    pub(super) async fn send(
        &mut self,
        listener: usize,
        packet: Datagram,
    ) -> Result<(), ControlError> {
        if packet.bytes.len() > MAX_MESSAGE {
            return Err(ControlError::Capacity);
        }
        if let ListenerSocket::Datagram(socket) = &self.listeners[listener] {
            let size = tokio::time::timeout(
                Duration::from_millis(100),
                socket.send_to(&packet.bytes, packet.peer),
            )
            .await
            .map_err(failure)?
            .map_err(failure)?;
            return if size == packet.bytes.len() {
                Ok(())
            } else {
                Err(ControlError::Adapter(
                    "Incomplete signaling datagram".into(),
                ))
            };
        }
        let port = self
            .ports
            .get_mut(&(listener, packet.peer))
            .ok_or(ControlError::Conflict("Signaling flow is not admitted"))?;
        match &port.state {
            ConnectionState::Open => {
                if let Err(error) = port.packets.try_send(packet.bytes) {
                    port.state = ConnectionState::Closing(failure(error));
                    port.stop.send_replace(true);
                }
            }
            ConnectionState::Closing(_) => {}
        }
        Ok(())
    }

    pub(super) async fn next(&mut self) -> Result<Input, ControlError> {
        loop {
            tokio::select! {
                event = self.prepared.recv() => {
                    if let Some(input) = self.prepared_event(event.ok_or(ControlError::Capacity)?) { return Ok(input); }
                },
                upgrade = self.upgrades.recv() => {
                    let request = upgrade.ok_or(ControlError::Capacity)?;
                    let result = self.upgrade(request.scheme, request.peer);
                    if request.reply.send(result).is_err() { tracing::debug!("Signaling upgrade requester closed"); }
                },
                received = listen(&self.listeners) => match received? {
                    ListenerInput::Datagram(packet) => return Ok(Input::Packet(packet)),
                    ListenerInput::Accepted { listener, peer, stream } => {
                        if let Err(error) = self.accepted(listener, peer, stream) { tracing::debug!(%error, "signaling connection refused"); }
                    },
                },
                packet = self.received.recv() => {
                    let packet = packet.ok_or(ControlError::Capacity)?;
                    if !self.ports.get(&(packet.key.listener, packet.key.peer)).is_some_and(|port| port.key == packet.key) { continue; }
                    match packet.item {
                        StreamEvent::Message(bytes) => {
                            if self.ports.get(&(packet.key.listener, packet.key.peer)).is_some_and(|port| matches!(port.state, ConnectionState::Open)) {
                                return Ok(Input::Packet(Received { listener: packet.key.listener, peer: packet.key.peer, bytes }));
                            }
                        },
                        StreamEvent::Finished(mut result) => {
                            if let Some(port) = self.ports.remove(&(packet.key.listener, packet.key.peer)) {
                                if let ConnectionState::Closing(error) = port.state { result = Err(error); }
                            }
                            return Ok(Input::Closed { key: packet.key, result });
                        },
                    }
                },
                result = self.jobs.join_next(), if !self.jobs.is_empty() => {
                    let result = result.ok_or(ControlError::Capacity)?.map_err(failure)?;
                    if let Err(error) = result { tracing::debug!(%error, "signaling stream cleanup failed"); }
                },
            }
        }
    }

    pub(super) fn refuse(&mut self, listener: usize, peer: SocketAddr) {
        if let Some(port) = self.ports.get_mut(&(listener, peer)) {
            port.state =
                ConnectionState::Closing(ControlError::Invalid("Invalid signaling packet"));
            port.stop.send_replace(true);
        }
    }

    pub(super) async fn close(&mut self) -> Result<(), ControlError> {
        self.upgrades.close();
        while let Ok(request) = self.upgrades.try_recv() {
            if request.reply.send(Err(ControlError::Capacity)).is_err() {
                tracing::debug!("Signaling upgrade requester stopped");
            }
        }
        for port in self.ports.values() {
            port.stop.send_replace(true);
        }
        self.close_preparations();
        self.prepared.close();
        self.received.close();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut failure = None;
        while !self.jobs.is_empty() {
            match tokio::time::timeout_at(deadline, self.jobs.join_next()).await {
                Ok(Some(Ok(Ok(())))) | Ok(None) => {}
                Ok(Some(Ok(Err(error)))) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                Ok(Some(Err(error))) => {
                    if failure.is_none() {
                        failure = Some(super::failure(error));
                    }
                }
                Err(error) => {
                    self.jobs.abort_all();
                    while let Some(result) = self.jobs.join_next().await {
                        if let Err(error) = result {
                            tracing::debug!(%error, "signaling connection task cancelled");
                        }
                    }
                    return Err(super::failure(error));
                }
            }
        }
        self.ports.clear();
        self.preparations.clear();
        self.setups.clear();
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

async fn emit(
    sender: &mpsc::Sender<StreamPacket>,
    packet: StreamPacket,
    stop: &mut watch::Receiver<bool>,
) -> Result<bool, ControlError> {
    if *stop.borrow() {
        return Ok(false);
    }
    tokio::select! {
        changed = stop.changed() => match changed { Ok(()) | Err(_) => Ok(false) },
        sent = tokio::time::timeout(Duration::from_millis(100), sender.send(packet)) => { sent.map_err(failure)?.map_err(failure)?; Ok(true) },
    }
}

async fn finished(
    sender: &mpsc::Sender<StreamPacket>,
    key: ConnectionKey,
    result: Result<(), ControlError>,
    stop: &watch::Receiver<bool>,
) -> Result<(), ControlError> {
    match sender
        .send(StreamPacket {
            key,
            item: StreamEvent::Finished(result),
        })
        .await
    {
        Ok(()) => Ok(()),
        Err(_) if *stop.borrow() => Ok(()),
        Err(error) => Err(failure(error)),
    }
}

async fn drain<S: AsyncWrite + Unpin>(
    socket: &mut S,
    packets: &mut mpsc::Receiver<Vec<u8>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<(), ControlError> {
    let mut watching = true;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            changed = stop.changed(), if watching => match changed {
                Ok(()) => { if *stop.borrow() { return Ok(()); } },
                Err(_) => watching = false,
            },
            packet = packets.recv() => match packet {
                Some(packet) => write(socket, &packet).await?,
                None => return Ok(()),
            },
        }
    }
}

async fn write<S: AsyncWrite + Unpin>(socket: &mut S, bytes: &[u8]) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_millis(100), async {
        socket.write_all(bytes).await?;
        socket.flush().await
    })
    .await
    .map_err(failure)?
    .map_err(failure)
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: S,
    key: ConnectionKey,
    mut packets: mpsc::Receiver<Vec<u8>>,
    sender: mpsc::Sender<StreamPacket>,
    mut stop: watch::Receiver<bool>,
    role: StreamRole,
) -> Result<(), ControlError> {
    let result = async {
        let mut decoder = SipStreamDecoder::new();
        let mut bytes = [0; 4096];
        let mut crlf = false;
        let mut deadline = match role {
            StreamRole::Accepted => FrameDeadline::FirstMessage(tokio::time::Instant::now() + Duration::from_secs(5)),
            StreamRole::Initiated => FrameDeadline::Idle,
        };
        loop {
            if *stop.borrow() { break; }
            let timer = async { match deadline { FrameDeadline::FirstMessage(deadline) | FrameDeadline::Message(deadline) => tokio::time::sleep_until(deadline).await, FrameDeadline::Idle => std::future::pending().await } };
            tokio::select! {
                changed = stop.changed() => if changed.is_err() || *stop.borrow() { break; },
                () = timer => return Err(ControlError::Invalid("Signaling frame timed out")),
                packet = packets.recv() => match packet { Some(packet) => write(&mut socket, &packet).await?, None => break },
                received = socket.read(&mut bytes) => {
                    let count = received.map_err(failure)?;
                    if count == 0 { decoder.finish().map_err(failure)?; break; }
                    if decoder.is_empty() && matches!(deadline, FrameDeadline::Idle) { deadline = FrameDeadline::Message(tokio::time::Instant::now() + Duration::from_secs(5)); }
                    let mut offset = 0;
                    let mut keepalives = 0;
                    while offset < count {
                        let (consumed, item) = decoder.push(&bytes[offset..count]).map_err(failure)?;
                        offset += consumed;
                        match item {
                            Some(StreamItem::Message(bytes)) => { crlf = false; deadline = FrameDeadline::Idle; if !emit(&sender, StreamPacket { key, item: StreamEvent::Message(bytes) }, &mut stop).await? { return Ok(()); } },
                            Some(StreamItem::Crlf) => { if crlf { keepalives += 1; if keepalives > 16 { return Err(ControlError::Capacity); } write(&mut socket, b"\r\n").await?; crlf = false; } else { crlf = true; } },
                            None => {},
                        }
                    }
                    if decoder.is_empty() {
                        if matches!(deadline, FrameDeadline::Message(_)) { deadline = FrameDeadline::Idle; }
                    } else if matches!(deadline, FrameDeadline::Idle) {
                        deadline = FrameDeadline::Message(tokio::time::Instant::now() + Duration::from_secs(5));
                    }
                }
            }
        }
        Ok(())
    }.await;
    let result = match finished(&sender, key, result, &stop).await {
        Ok(()) => drain(&mut socket, &mut packets, &mut stop).await,
        Err(error) => Err(error),
    };
    let cleanup = tokio::time::timeout(Duration::from_millis(100), socket.shutdown())
        .await
        .map_err(failure)
        .and_then(|result| result.map_err(failure));
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
            "{primary}; stream cleanup failed: {cleanup}"
        ))),
    }
}
