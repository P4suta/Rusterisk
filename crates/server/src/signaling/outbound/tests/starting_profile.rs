use super::{
    failure, Config, ControlError, Controller, DestinationResolver, Effect, LookupIpStrategy,
    MediaHub, Message, NameServerConfig, OpCode, Protocol, RData, Record, ResolveHosts,
    ResolverConfig, ResolverOpts, Signaling, A,
};
use crate::signaling::calls;
use crate::{
    signaling::{cleanup, transport::Input},
    Action, CallKey, Credentials,
};
use rsipstack::sip::{
    headers::{auth::AuthQop, typed::Authorization},
    HeadersExt, Method, Request, SipMessage,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Codec;
use rusterisk_sip::{Engine, Session, SipStreamDecoder, StreamItem};
use rusterisk_test_support::Wait;
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::JoinHandle,
};

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

fn configuration(protocol: Protocol, uri: &str, replaced: bool) -> Result<Config, ControlError> {
    let token = match protocol {
        Protocol::Udp => "udp",
        Protocol::Tcp => "tcp",
        Protocol::Tls | Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
    };
    let (username, password, codec) = if replaced {
        ("replacement-user", "replacement-password", "alaw")
    } else {
        ("captured-user", "captured-password", "ulaw")
    };
    let contact = uri.replace(';', "\\;");
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[transport]\ntype=transport\nprotocol={token}\nbind=127.0.0.1:0\n[incoming]\ntype=auth\nusername=incoming\npassword=sample\n[remote]\ntype=auth\nusername={username}\npassword={password}\nrealm=profile.test\n[phone]\ntype=aor\ncontact={contact}\n[phone]\ntype=endpoint\ntransport=transport\nauth=incoming\noutbound_auth=remote\naors=phone\nallow={codec}\n"),
        "",
        "",
    ).map_err(failure)?;
    config.rtp.start = 0;
    config.rtp.end = 0;
    require(
        config.aors.first().and_then(|aor| aor.contact.as_deref()) == Some(uri),
        "Starting profile fixture lost its configured contact URI",
    )?;
    Ok(config)
}

#[test]
fn starting_profile_fixtures_use_os_assigned_media_ports() -> Result<(), ControlError> {
    let config = configuration(Protocol::Udp, "sip:phone@127.0.0.1:5060", false)?;
    require(
        (config.rtp.start, config.rtp.end) == (0, 0),
        "Starting profile fixture shares the default media port range",
    )
}

struct Packet {
    request: Request,
    uri: String,
    source: SocketAddr,
}

impl Packet {
    fn parse(bytes: &[u8], source: SocketAddr) -> Result<Self, ControlError> {
        let line = bytes
            .split(|byte| *byte == b'\r')
            .next()
            .ok_or(ControlError::Capacity)?;
        let uri = std::str::from_utf8(line)
            .map_err(failure)?
            .split_whitespace()
            .nth(1)
            .ok_or(ControlError::Capacity)?
            .to_owned();
        let SipMessage::Request(request) = SipMessage::try_from(bytes).map_err(failure)? else {
            return Err(ControlError::Invalid("Expected native outgoing request"));
        };
        Ok(Self {
            request,
            uri,
            source,
        })
    }
}

enum PeerSocket {
    Udp(UdpSocket),
    Tcp {
        listener: TcpListener,
        stream: Option<TcpStream>,
    },
}

struct Peer {
    socket: PeerSocket,
    decoder: SipStreamDecoder,
}

impl Peer {
    async fn new(protocol: Protocol) -> Result<Self, ControlError> {
        let socket = match protocol {
            Protocol::Udp => {
                PeerSocket::Udp(UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?)
            }
            Protocol::Tcp => PeerSocket::Tcp {
                listener: TcpListener::bind("127.0.0.1:0").await.map_err(failure)?,
                stream: None,
            },
            Protocol::Tls | Protocol::Ws | Protocol::Wss => return Err(ControlError::Unsupported),
        };
        Ok(Self {
            socket,
            decoder: SipStreamDecoder::new(),
        })
    }

    fn address(&self) -> Result<SocketAddr, ControlError> {
        match &self.socket {
            PeerSocket::Udp(socket) => socket.local_addr().map_err(failure),
            PeerSocket::Tcp { listener, .. } => listener.local_addr().map_err(failure),
        }
    }

    async fn accept(&mut self) -> Result<(), ControlError> {
        if let PeerSocket::Tcp { listener, stream } = &mut self.socket {
            let (socket, _) = Wait::Ready
                .bounded(listener.accept())
                .await
                .map_err(failure)?
                .map_err(failure)?;
            *stream = Some(socket);
        }
        Ok(())
    }

    async fn receive(&mut self) -> Result<Packet, ControlError> {
        Wait::Ready
            .bounded(async {
                match &mut self.socket {
                    PeerSocket::Udp(socket) => {
                        let mut bytes = [0; rusterisk_sip::MAX_MESSAGE + 1];
                        let (size, source) = socket.recv_from(&mut bytes).await.map_err(failure)?;
                        Packet::parse(&bytes[..size], source)
                    }
                    PeerSocket::Tcp { stream, .. } => {
                        let socket = stream
                            .as_mut()
                            .ok_or(ControlError::Invalid("Expected one owned native TCP flow"))?;
                        let source = socket.peer_addr().map_err(failure)?;
                        let mut byte = [0; 1];
                        loop {
                            if socket.read(&mut byte).await.map_err(failure)? == 0 {
                                return Err(ControlError::Invalid(
                                    "Native TCP flow closed before request",
                                ));
                            }
                            match self.decoder.push(&byte).map_err(failure)?.1 {
                                Some(StreamItem::Message(bytes)) => {
                                    return Packet::parse(&bytes, source)
                                }
                                Some(StreamItem::Crlf) | None => {}
                            }
                        }
                    }
                }
            })
            .await
            .map_err(failure)?
    }

    async fn send(&mut self, bytes: &[u8], target: SocketAddr) -> Result<(), ControlError> {
        match &mut self.socket {
            PeerSocket::Udp(socket) => require(
                socket.send_to(bytes, target).await.map_err(failure)? == bytes.len(),
                "Short native UDP challenge",
            ),
            PeerSocket::Tcp { stream, .. } => {
                let socket = stream.as_mut().ok_or(ControlError::Capacity)?;
                require(
                    socket.peer_addr().map_err(failure)? == target,
                    "Native challenge changed TCP flow",
                )?;
                socket.write_all(bytes).await.map_err(failure)
            }
        }
    }
}

struct Fixture {
    signaling: Signaling,
    hub: MediaHub,
    subscription: Option<crate::Subscription>,
    protocol: Protocol,
    local: SocketAddr,
    rtp: Option<SocketAddr>,
}

impl Fixture {
    async fn new(config: Config, protocol: Protocol) -> Result<Self, ControlError> {
        let (controller, runner, _effects) = Controller::new(config.clone(), 8);
        let (media, hub, notices) = MediaHub::new(8);
        let signaling = Signaling::bind(config, controller, runner, media, notices).await?;
        let local = signaling.network.local_addr(0)?;
        Ok(Self {
            signaling,
            hub,
            subscription: None,
            protocol,
            local,
            rtp: None,
        })
    }

    async fn accepted(&mut self) -> Result<(Effect, CallKey), ControlError> {
        let controller = self.signaling.controller.clone();
        self.subscription = Some(
            self.signaling
                .control_until(controller.subscribe(
                    Credentials::new("app".into(), "test".into())?,
                    None,
                    "app".into(),
                ))
                .await??,
        );
        self.signaling
            .control_until(controller.execute(
                Credentials::new("app".into(), "test".into())?,
                None,
                Action::Originate {
                    id: ChannelId::try_from("starting-profile".to_owned())?,
                    endpoint: "PJSIP/phone".into(),
                    app: "app".into(),
                    args: Vec::new(),
                    caller: "displayed-caller".into(),
                    timeout: 30,
                },
            ))
            .await??;
        let effect = Wait::Ready
            .bounded(self.signaling.next_effect_for_test())
            .await
            .map_err(failure)??;
        let Effect::Start { key, .. } = &effect else {
            return Err(ControlError::Invalid("Expected accepted native start"));
        };
        let key = key.clone();
        Ok((effect, key))
    }

    async fn resolved(&mut self) -> Result<(), ControlError> {
        let completed = Wait::Ready
            .bounded(self.signaling.native_job_for_test())
            .await
            .map_err(failure)??
            .ok_or(ControlError::Invalid(
                "Accepted starting profile did not retain owned resolution",
            ))?
            .map_err(failure)?;
        self.signaling.native_completed(completed, 1).await
    }

    async fn connected(&mut self, key: &CallKey) -> Result<(), ControlError> {
        Wait::Ready
            .bounded(async {
                for _ in 0..16 {
                    if self.signaling.records.get(calls::ACTIVE, key).is_some() {
                        return Ok(());
                    }
                    self.input(2).await?;
                }
                Err(ControlError::Capacity)
            })
            .await
            .map_err(failure)?
    }

    async fn input(&mut self, now: u64) -> Result<bool, ControlError> {
        let input = Wait::Ready
            .bounded(self.signaling.network.next())
            .await
            .map_err(failure)??;
        match input {
            Input::Prepared(prepared) => self
                .signaling
                .native_prepared(prepared, now)
                .await
                .map(|()| false),
            Input::SetupFinished { key, result } => self
                .signaling
                .native_setup_finished(key, result)
                .await
                .map(|()| false),
            Input::Packet(packet) => {
                let engine = &mut self.signaling.listeners[packet.listener].engine;
                let output = match packet.route().flow()? {
                    Some(flow) => engine.receive_on_flow(&packet.bytes, packet.peer, flow, now),
                    None => engine.receive(&packet.bytes, packet.peer, now),
                }
                .map_err(failure)?;
                self.signaling
                    .output(packet.listener, output, now)
                    .await
                    .map(|()| true)
            }
            Input::Closed { result, .. } => result.and(Err(ControlError::Invalid(
                "Native profile flow closed before challenge completion",
            ))),
            Input::Upgrade(_) => Err(ControlError::Invalid("Unexpected native profile upgrade")),
        }
    }

    async fn finish(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let controller = self.signaling.controller.clone();
        let stop = controller.shutdown_signal();
        controller.shutdown();
        let mut result = cleanup::combine(
            primary,
            joined(tokio::spawn(self.signaling.run(stop))).await,
        );
        drop(self.subscription);
        drop(self.hub);
        let rebound = match self.protocol {
            Protocol::Udp => UdpSocket::bind(self.local).await.map(drop).map_err(failure),
            Protocol::Tcp => TcpListener::bind(self.local)
                .await
                .map(drop)
                .map_err(failure),
            Protocol::Tls | Protocol::Ws | Protocol::Wss => Err(ControlError::Unsupported),
        };
        result = cleanup::combine(result, rebound);
        if let Some(address) = self.rtp {
            let control = address.port().checked_add(1).ok_or(ControlError::Capacity);
            let rebound = match control {
                Ok(control) => crate::rtp::SocketPair::bind(address.ip(), address.port(), control)
                    .map(drop)
                    .map_err(failure),
                Err(error) => Err(error),
            };
            result = cleanup::combine(result, rebound);
        }
        result
    }
}

async fn joined(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            let reaped = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
                Ok(result) => result,
            };
            cleanup::combine(Err(failure(timeout)), reaped)
        }
    }
}

fn original(fixture: &mut Fixture, packet: &Packet, uri: &str) -> Result<(), ControlError> {
    let session = Session::parse(
        std::str::from_utf8(&packet.request.body).map_err(failure)?,
        &[Codec::Pcmu, Codec::Pcma],
    )
    .map_err(failure)?;
    fixture.rtp = Some(session.remote);
    if packet.request.method != Method::Invite || packet.uri != uri {
        return Err(failure(format!(
            "Accepted native request mismatch: expected method=INVITE uri={uri}; actual method={} uri={}",
            packet.request.method, packet.uri
        )));
    }
    require(
        session.codec == Codec::Pcmu && session.payload_type == 0,
        "Accepted native call changed its starting codec",
    )?;
    let local = fixture.local.to_string();
    require(
        packet
            .request
            .top_via_header()
            .map_err(failure)?
            .value()
            .contains(&local)
            && packet
                .request
                .contact_header()
                .map_err(failure)?
                .value()
                .contains(&local),
        "Captured native profile did not retain the actual bound SIP address",
    )?;
    require(
        packet
            .request
            .from_header()
            .map_err(failure)?
            .value()
            .contains("displayed-caller"),
        "Displayed caller was not preserved separately from authentication",
    )
}

fn hash(source: &[u8]) -> String {
    Sha256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn authenticated(
    fixture: &mut Fixture,
    peer: &mut Peer,
    initial: Packet,
) -> Result<(), ControlError> {
    let to = format!(
        "{};tag=profile-peer",
        initial.request.to_header().map_err(failure)?.value()
    );
    let challenge = format!("SIP/2.0 401 Unauthorized\r\nVia: {}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {}\r\nWWW-Authenticate: Digest realm=\"profile.test\", nonce=\"profile-nonce\", algorithm=SHA-256, qop=\"auth-int\"\r\nContent-Length: 0\r\n\r\n", initial.request.top_via_header().map_err(failure)?.value(), initial.request.from_header().map_err(failure)?.value(), initial.request.call_id_header().map_err(failure)?.value(), initial.request.cseq_header().map_err(failure)?.value());
    peer.send(challenge.as_bytes(), initial.source).await?;
    let mut received = false;
    for _ in 0..16 {
        if fixture.input(3).await? {
            received = true;
            break;
        }
    }
    require(received, "Native flow did not process the actual challenge")?;
    let ack = peer.receive().await?;
    require(
        ack.request.method == Method::Ack
            && ack.request.top_via_header().map_err(failure)?
                == initial.request.top_via_header().map_err(failure)?
            && ack.request.call_id_header().map_err(failure)?
                == initial.request.call_id_header().map_err(failure)?
            && ack.request.from_header().map_err(failure)?
                == initial.request.from_header().map_err(failure)?
            && ack.request.to_header().map_err(failure)?.value() == to
            && ack.request.cseq_header().map_err(failure)?.value() == "1 ACK",
        "Native authentication ACK changed its transaction identity",
    )?;
    let retried = peer.receive().await?;
    require(
        retried.request.method == Method::Invite
            && retried.uri == initial.uri
            && retried.request.body == initial.request.body
            && retried.request.cseq_header().map_err(failure)?.value() == "2 INVITE",
        "Native authentication retry changed its captured request",
    )?;
    let authorization = Authorization::parse(
        retried
            .request
            .authorization_header()
            .ok_or(ControlError::Invalid(
                "Missing captured outbound authentication",
            ))?
            .value(),
    )
    .map_err(failure)?;
    let Some(AuthQop::AuthInt { cnonce, nc: 1 }) = &authorization.qop else {
        return Err(ControlError::Invalid(
            "Expected captured auth-int challenge response",
        ));
    };
    let a1 = hash(b"captured-user:profile.test:captured-password");
    let a2 = hash(
        format!(
            "{}:{}:{}",
            retried.request.method,
            retried.uri,
            hash(&retried.request.body)
        )
        .as_bytes(),
    );
    let expected = hash(format!("{a1}:profile-nonce:00000001:{cnonce}:auth-int:{a2}").as_bytes());
    require(
        authorization.username == "captured-user"
            && authorization.realm == "profile.test"
            && authorization.response == expected,
        "Native authentication did not use the accepted starting credentials",
    )
}

#[tokio::test]
async fn accepted_udp_start_survives_endpoint_removal_before_dispatch() -> Result<(), ControlError>
{
    let mut peer = Peer::new(Protocol::Udp).await?;
    let address = peer.address()?;
    let uri = format!("sip:captured@{address}");
    let mut fixture =
        Fixture::new(configuration(Protocol::Udp, &uri, false)?, Protocol::Udp).await?;
    let primary = Wait::Scenario
        .bounded(async {
            let (effect, key) = fixture.accepted().await?;
            fixture.signaling.config.endpoints.clear();
            fixture.signaling.effect(effect, 0).await?;
            fixture.resolved().await?;
            require(
                fixture.signaling.records.get(calls::ACTIVE, &key).is_some(),
                "Accepted native profile was lost before dispatch",
            )?;
            let initial = peer.receive().await?;
            original(&mut fixture, &initial, &uri)?;
            authenticated(&mut fixture, &mut peer, initial).await
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
    let result = fixture.finish(primary).await;
    drop(peer);
    cleanup::combine(
        result,
        UdpSocket::bind(address).await.map(drop).map_err(failure),
    )
}

#[tokio::test]
async fn a_dns_wait_keeps_the_accepted_profile_when_current_native_settings_are_replaced(
) -> Result<(), ControlError> {
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let dns_address = dns.local_addr().map_err(failure)?;
    let mut peer = Peer::new(Protocol::Tcp).await?;
    let address = peer.address()?;
    let replacement = Peer::new(Protocol::Tcp).await?;
    let replacement_address = replacement.address()?;
    let uri = format!("sip:captured@peer.test:{};transport=TCP", address.port());
    let mut fixture =
        Fixture::new(configuration(Protocol::Tcp, &uri, false)?, Protocol::Tcp).await?;
    let primary = Wait::Ready
        .bounded(async {
            let mut server = NameServerConfig::udp(Ipv4Addr::LOCALHOST.into());
            server
                .connections
                .first_mut()
                .ok_or(ControlError::Capacity)?
                .port = dns_address.port();
            let mut options = ResolverOpts::default();
            options.ip_strategy = LookupIpStrategy::Ipv4Only;
            options.use_hosts_file = ResolveHosts::Never;
            fixture.signaling.resolver = DestinationResolver::with_config(
                ResolverConfig::from_name_servers(std::iter::once(server).collect()),
                options,
            );
            let (effect, key) = fixture.accepted().await?;
            fixture.signaling.effect(effect, 0).await?;
            let mut bytes = [0; 2048];
            let (size, client) = fixture
                .signaling
                .control_until(Wait::Ready.bounded(dns.recv_from(&mut bytes)))
                .await?
                .map_err(failure)?
                .map_err(failure)?;
            let request = Message::from_vec(&bytes[..size]).map_err(failure)?;
            let query = request
                .queries
                .first()
                .ok_or(ControlError::Capacity)?
                .clone();
            require(
                query.name().to_string() == "peer.test.",
                "DNS did not use the accepted starting destination",
            )?;
            let mut current = configuration(
                Protocol::Tcp,
                &format!("sip:replacement@{replacement_address};transport=tcp"),
                true,
            )?;
            current
                .transports
                .first_mut()
                .ok_or(ControlError::Capacity)?
                .bind = fixture
                .signaling
                .config
                .transports
                .first()
                .ok_or(ControlError::Capacity)?
                .bind;
            fixture.signaling.listeners[0].engine =
                Engine::new(current.clone(), [21; 32]).with_transport(Protocol::Tcp);
            fixture.signaling.config = current;
            let mut reply = Message::response(request.metadata.id, OpCode::Query);
            reply.metadata.authoritative = true;
            reply.metadata.recursion_available = true;
            reply.metadata.recursion_desired = request.metadata.recursion_desired;
            reply.queries.push(query.clone());
            reply.answers.push(Record::from_rdata(
                query.name().clone(),
                60,
                RData::A(A(Ipv4Addr::LOCALHOST)),
            ));
            reply.edns = request.edns;
            let bytes = reply.to_vec().map_err(failure)?;
            require(
                dns.send_to(&bytes, client).await.map_err(failure)? == bytes.len(),
                "Short DNS response",
            )?;
            fixture.resolved().await?;
            fixture.connected(&key).await?;
            peer.accept().await?;
            let initial = peer.receive().await?;
            original(&mut fixture, &initial, &uri)?;
            authenticated(&mut fixture, &mut peer, initial).await
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
    let mut result = fixture.finish(primary).await;
    drop((peer, replacement, dns));
    for address in [address, replacement_address] {
        result = cleanup::combine(
            result,
            TcpListener::bind(address).await.map(drop).map_err(failure),
        );
    }
    cleanup::combine(
        result,
        UdpSocket::bind(dns_address)
            .await
            .map(drop)
            .map_err(failure),
    )
}
