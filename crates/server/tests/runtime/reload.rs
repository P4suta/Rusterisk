use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Empty};
use hyper::{body::Bytes, client::conn::http1::SendRequest, Request};
use hyper_util::rt::TokioIo;
use reqwest::{Client, StatusCode};
use rusterisk_api::{Channel, Event, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{rtp::SocketPair, runtime::Server, ControlError, Controller};
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Error, Message},
    MaybeTlsStream, WebSocketStream,
};

type Events = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy)]
struct Access {
    password: &'static str,
    origin: &'static str,
}

const OLD: Access = Access {
    password: "test-password",
    origin: "https://old.example",
};
const REPLACEMENT: Access = Access {
    password: "replacement-password",
    origin: "https://replacement.example",
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn verify(condition: bool, reason: &'static str) -> Result<(), ControlError> {
    condition.then_some(()).ok_or(ControlError::Invalid(reason))
}

fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; runtime reload cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(20), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            let result = Err(failure(primary));
            match task.await {
                Err(error) if error.is_cancelled() => result,
                Err(error) => combine(result, Err(failure(error))),
                Ok(cleanup) => combine(result, cleanup),
            }
        }
    }
}

fn configuration(access: Access) -> Result<Config, ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &format!(
            "[general]\nallowed_origins={}\n[app]\ntype=user\npassword={}\n",
            access.origin, access.password
        ),
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    Ok(config)
}

fn authorization(access: Access) -> Result<HeaderValue, ControlError> {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("app:{}", access.password));
    let mut value = HeaderValue::from_str(&format!("Basic {encoded}")).map_err(failure)?;
    value.set_sensitive(true);
    Ok(value)
}

struct KeepAlive {
    address: SocketAddr,
    sender: SendRequest<Empty<Bytes>>,
    connection: JoinHandle<Result<(), ControlError>>,
}

impl KeepAlive {
    async fn connect(address: SocketAddr) -> Result<Self, ControlError> {
        let (sender, connection) = tokio::time::timeout(Duration::from_secs(2), async {
            let stream = TcpStream::connect(address).await.map_err(failure)?;
            hyper::client::conn::http1::handshake(TokioIo::new(stream))
                .await
                .map_err(failure)
        })
        .await
        .map_err(failure)??;
        Ok(Self {
            address,
            sender,
            connection: tokio::spawn(async move { connection.await.map_err(failure) }),
        })
    }

    async fn ping(&mut self, access: Access) -> Result<StatusCode, ControlError> {
        tokio::time::timeout(Duration::from_secs(2), async {
            let request = Request::builder()
                .uri("/ari/asterisk/ping")
                .header("Host", self.address.to_string())
                .header("Origin", access.origin)
                .header("Authorization", authorization(access)?)
                .body(Empty::new())
                .map_err(failure)?;
            let response = self.sender.send_request(request).await.map_err(failure)?;
            let status = response.status();
            let mut body = response.into_body();
            let mut size = 0usize;
            while let Some(frame) = body.frame().await {
                if let Some(data) = frame.map_err(failure)?.data_ref() {
                    size = size.checked_add(data.len()).ok_or(ControlError::Capacity)?;
                    if size > 16_384 {
                        return Err(ControlError::Capacity);
                    }
                }
            }
            Ok(status)
        })
        .await
        .map_err(failure)?
    }

    async fn close(self) -> Result<(), ControlError> {
        let Self {
            sender, connection, ..
        } = self;
        drop(sender);
        joined(connection).await
    }
}

async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failure)? {
        let size = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(ControlError::Capacity)?;
        if size > 16_384 {
            return Err(ControlError::Capacity);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn close_events(socket: &mut Events) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        socket.close(None).await.map_err(failure)?;
        for _ in 0..64 {
            match socket.next().await.transpose().map_err(failure)? {
                Some(Message::Close(_)) => {
                    return verify(
                        socket.next().await.transpose().map_err(failure)?.is_none(),
                        "Event connection remained open after Close",
                    );
                }
                Some(Message::Text(_) | Message::Pong(_) | Message::Ping(_)) => {}
                Some(Message::Binary(_) | Message::Frame(_)) | None => {
                    return Err(ControlError::Invalid("Event Close response missing"));
                }
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

struct Fixture {
    initial: Config,
    address: SocketAddr,
    sip: SocketAddr,
    controller: Controller,
    server: JoinHandle<Result<(), ControlError>>,
    http: Client,
    peer: SocketPair,
    peer_addresses: [SocketAddr; 2],
    events: Vec<Events>,
    keepalive: Option<KeepAlive>,
}

impl Fixture {
    async fn start(data: &Path) -> Result<Self, ControlError> {
        let http = Client::builder()
            .no_proxy()
            .http1_only()
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(failure)?;
        let initial = configuration(OLD)?;
        let peer =
            SocketPair::bind("127.0.0.1".parse().map_err(failure)?, 0, 0).map_err(failure)?;
        let peer_addresses = [
            peer.local_addr().map_err(failure)?,
            peer.control_addr().map_err(failure)?,
        ];
        let server = Server::bind(initial.clone(), 4, data.to_owned()).await?;
        let locations = server.local_addr().and_then(|address| {
            let listeners = server.sip_listeners()?;
            let [(name, sip)] = listeners.as_slice() else {
                return Err(ControlError::Invalid("Expected one SIP listener"));
            };
            verify(name == "udp", "SIP listener identity changed")?;
            Ok((address, *sip))
        });
        let controller = server.controller();
        let task = tokio::spawn(server.run());
        let (address, sip) = match locations {
            Ok(locations) => locations,
            Err(error) => {
                controller.shutdown();
                return combine(Err(error), joined(task).await);
            }
        };
        Ok(Self {
            initial,
            address,
            sip,
            controller,
            server: task,
            http,
            peer,
            peer_addresses,
            events: Vec::with_capacity(2),
            keepalive: None,
        })
    }

    async fn ping(&self, access: Access) -> Result<StatusCode, ControlError> {
        let response = self
            .http
            .get(format!("http://{}/ari/asterisk/ping", self.address))
            .basic_auth("app", Some(access.password))
            .header("Origin", access.origin)
            .send()
            .await
            .map_err(failure)?;
        let status = response.status();
        body(response).await?;
        Ok(status)
    }

    async fn check_access(
        &mut self,
        access: Access,
        expected: StatusCode,
    ) -> Result<(), ControlError> {
        let actual = self.ping(access).await?;
        if actual != expected {
            return Err(failure(format!(
                "HTTP access returned {actual}; expected {expected}"
            )));
        }
        let actual = self
            .keepalive
            .as_mut()
            .ok_or(ControlError::Capacity)?
            .ping(access)
            .await?;
        if actual != expected {
            return Err(failure(format!(
                "Established HTTP access returned {actual}; expected {expected}"
            )));
        }
        Ok(())
    }

    async fn event_connection(
        &mut self,
        application: &str,
        access: Access,
        expected: StatusCode,
    ) -> Result<(), ControlError> {
        let mut request = format!("ws://{}/ari/events?app={application}", self.address)
            .into_client_request()
            .map_err(failure)?;
        request
            .headers_mut()
            .insert("Authorization", authorization(access)?);
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_static(access.origin));
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("ari"));
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(failure)?;
        match result {
            Ok((mut socket, response)) => {
                let admitted = verify(
                    expected == StatusCode::SWITCHING_PROTOCOLS
                        && response.status() == expected
                        && response.headers().get("Sec-WebSocket-Protocol")
                            == Some(&HeaderValue::from_static("ari")),
                    "Event upgrade did not enforce the current access policy",
                );
                if admitted.is_err() || self.events.len() >= 2 {
                    return combine(
                        admitted.and(Err(ControlError::Capacity)),
                        close_events(&mut socket).await,
                    );
                }
                self.events.push(socket);
                Ok(())
            }
            Err(Error::Http(response)) => verify(
                response.status() == expected && expected != StatusCode::SWITCHING_PROTOCOLS,
                "Unexpected event upgrade rejection",
            ),
            Err(error) => Err(failure(error)),
        }
    }

    async fn external(
        &mut self,
        application: &str,
        channel: &str,
        access: Access,
        observer: usize,
    ) -> Result<(), ControlError> {
        let remote = self.peer.local_addr().map_err(failure)?.to_string();
        let mut url = reqwest::Url::parse(&format!(
            "http://{}/ari/channels/externalMedia",
            self.address
        ))
        .map_err(failure)?;
        url.query_pairs_mut().extend_pairs([
            ("app", application),
            ("channelId", channel),
            ("format", "ulaw"),
            ("external_host", remote.as_str()),
        ]);
        let response = self
            .http
            .post(url)
            .basic_auth("app", Some(access.password))
            .header("Origin", access.origin)
            .send()
            .await
            .map_err(failure)?;
        verify(
            response.status() == StatusCode::OK,
            "External media admission failed",
        )?;
        let snapshot: Channel = serde_json::from_slice(&body(response).await?)?;
        verify(
            snapshot.id.as_str() == channel,
            "External channel identity changed",
        )?;
        let events = self
            .events
            .get_mut(observer)
            .ok_or(ControlError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..64 {
                match events.next().await.transpose().map_err(failure)? {
                    Some(Message::Text(text)) => {
                        let event: Event = serde_json::from_str(&text)?;
                        if matches!(event.kind, EventKind::StasisStart { channel: observed, .. } if observed.id.as_str() == channel) {
                            return verify(event.application == application, "Stasis event reached another application");
                        }
                    }
                    Some(Message::Ping(bytes)) => events.send(Message::Pong(bytes)).await.map_err(failure)?,
                    Some(Message::Pong(_)) => {}
                    Some(Message::Binary(_) | Message::Close(_) | Message::Frame(_)) | None => {
                        return Err(ControlError::Invalid("Established event session ended during reload"));
                    }
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)?
    }

    async fn sip_options(&self, identity: &str) -> Result<(), ControlError> {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(failure)?;
        let local = socket.local_addr().map_err(failure)?;
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let packet = format!("OPTIONS sip:probe@{} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-reload-{identity}\r\nFrom: <sip:probe@localhost>;tag=probe\r\nTo: <sip:probe@localhost>\r\nCall-ID: reload-{identity}\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.sip);
            let sent = socket.send_to(packet.as_bytes(), self.sip).await.map_err(failure)?;
            verify(sent == packet.len(), "Incomplete SIP probe")?;
            let mut bytes = [0; 16_385];
            let (size, peer) = socket.recv_from(&mut bytes).await.map_err(failure)?;
            let response = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
            verify(peer == self.sip && response.starts_with("SIP/2.0 200 ")
                && response.contains(&format!("Call-ID: reload-{identity}\r\n")), "Original SIP listener stopped responding")
        }).await.map_err(failure).and_then(|result| result);
        drop(socket);
        combine(
            result,
            std::net::UdpSocket::bind(local).map(drop).map_err(failure),
        )
    }

    async fn finish(
        self,
        primary: Result<(), ControlError>,
        data: &Path,
    ) -> Result<(), ControlError> {
        let Self {
            controller,
            server,
            http,
            peer,
            peer_addresses,
            mut events,
            keepalive,
            address,
            sip,
            ..
        } = self;
        let mut result = primary;
        for socket in &mut events {
            result = combine(result, close_events(socket).await);
        }
        drop(events);
        if let Some(keepalive) = keepalive {
            result = combine(result, keepalive.close().await);
        }
        drop(http);
        controller.shutdown();
        result = combine(result, joined(server).await);
        drop(peer);
        for release in [
            std::net::TcpListener::bind(address)
                .map(drop)
                .map_err(failure),
            std::net::UdpSocket::bind(sip).map(drop).map_err(failure),
            rusterisk_storage::Store::open(data)
                .map(drop)
                .map_err(failure),
        ] {
            result = combine(result, release);
        }
        for address in peer_addresses {
            result = combine(
                result,
                std::net::UdpSocket::bind(address)
                    .map(drop)
                    .map_err(failure),
            );
        }
        result
    }
}

async fn publish(fixture: &mut Fixture) -> Result<(), ControlError> {
    verify(
        fixture.initial.http.bind.port() == 0
            && fixture
                .initial
                .transports
                .first()
                .is_some_and(|transport| transport.bind.port() == 0),
        "The fixture lost requested ephemeral listener settings",
    )?;
    fixture.keepalive = Some(KeepAlive::connect(fixture.address).await?);
    fixture.check_access(OLD, StatusCode::OK).await?;
    fixture
        .event_connection("existing-session", OLD, StatusCode::SWITCHING_PROTOCOLS)
        .await?;
    fixture
        .external("existing-session", "before-reload", OLD, 0)
        .await?;
    fixture.sip_options("before-publication").await?;
    let replacement = configuration(REPLACEMENT)?;
    fixture.controller.reload(replacement).await?;
    let old_password = Access {
        password: OLD.password,
        origin: REPLACEMENT.origin,
    };
    let old_origin = Access {
        password: REPLACEMENT.password,
        origin: OLD.origin,
    };
    for (access, expected) in [
        (old_password, StatusCode::UNAUTHORIZED),
        (old_origin, StatusCode::FORBIDDEN),
        (REPLACEMENT, StatusCode::OK),
    ] {
        fixture.check_access(access, expected).await?;
    }
    fixture
        .external("existing-session", "after-reload", REPLACEMENT, 0)
        .await?;
    fixture
        .event_connection("denied-origin", old_origin, StatusCode::FORBIDDEN)
        .await?;
    fixture
        .event_connection("denied-password", old_password, StatusCode::UNAUTHORIZED)
        .await?;
    fixture
        .event_connection(
            "replacement-session",
            REPLACEMENT,
            StatusCode::SWITCHING_PROTOCOLS,
        )
        .await?;
    fixture
        .external("replacement-session", "replacement-channel", REPLACEMENT, 1)
        .await?;
    fixture.sip_options("after-publication").await
}

async fn reject(fixture: &mut Fixture) -> Result<(), ControlError> {
    fixture.keepalive = Some(KeepAlive::connect(fixture.address).await?);
    fixture
        .event_connection("preserved-session", OLD, StatusCode::SWITCHING_PROTOCOLS)
        .await?;
    let invalid_music = configuration(REPLACEMENT)?
        .with_music_on_hold_configuration(
            "[unused]\nmode=files\ndirectory=music/invalid\nsort=alpha\n",
        )
        .map_err(failure)?;
    let mut changed_http = configuration(REPLACEMENT)?;
    changed_http
        .http
        .bind
        .set_ip("127.0.0.2".parse().map_err(failure)?);
    let mut changed_sip = configuration(REPLACEMENT)?;
    changed_sip
        .transports
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .bind
        .set_ip("127.0.0.2".parse().map_err(failure)?);
    for (replacement, channel) in [
        (invalid_music, "invalid-music"),
        (changed_http, "changed-http"),
        (changed_sip, "changed-sip"),
    ] {
        verify(
            fixture.controller.reload(replacement).await.is_err(),
            "Invalid replacement was published",
        )?;
        for (access, expected) in [
            (OLD, StatusCode::OK),
            (
                Access {
                    password: REPLACEMENT.password,
                    origin: OLD.origin,
                },
                StatusCode::UNAUTHORIZED,
            ),
            (
                Access {
                    password: OLD.password,
                    origin: REPLACEMENT.origin,
                },
                StatusCode::FORBIDDEN,
            ),
        ] {
            fixture.check_access(access, expected).await?;
        }
        fixture
            .external("preserved-session", channel, OLD, 0)
            .await?;
        fixture.sip_options(channel).await?;
    }
    fixture
        .event_connection("after-rejection", OLD, StatusCode::SWITCHING_PROTOCOLS)
        .await?;
    fixture
        .event_connection(
            "still-denied",
            Access {
                password: OLD.password,
                origin: REPLACEMENT.origin,
            },
            StatusCode::FORBIDDEN,
        )
        .await
}

enum Scenario {
    Publication,
    Rejection,
}

async fn run(data: &Path, scenario: Scenario) -> Result<(), ControlError> {
    std::fs::create_dir_all(data.join("music/invalid")).map_err(failure)?;
    std::fs::write(data.join("music/invalid/bad.wav"), b"invalid wave asset").map_err(failure)?;
    let mut fixture = Fixture::start(data).await?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        match scenario {
            Scenario::Publication => publish(&mut fixture).await,
            Scenario::Rejection => reject(&mut fixture).await,
        }
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    fixture.finish(result, data).await
}

#[tokio::test]
async fn reload_publishes_new_access_on_existing_http_and_preserves_established_event_sessions(
) -> Result<(), ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let result = run(data.path(), Scenario::Publication).await;
    combine(result, data.close().map_err(failure))
}

#[tokio::test]
async fn rejected_music_and_listener_replacements_preserve_current_access_and_bound_resources(
) -> Result<(), ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let result = run(data.path(), Scenario::Rejection).await;
    combine(result, data.close().map_err(failure))
}
