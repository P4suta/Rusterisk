use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::{body::Bytes, client::conn::http1::SendRequest, Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use rusterisk_api::{Channel, Event, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{runtime::Server, ControlError, Controller};
use std::{io, net::SocketAddr, path::Path, time::Duration};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    MaybeTlsStream, WebSocketStream,
};

const RELOAD: &str = "/rusterisk/configuration/reload";
const PING: &str = "/ari/asterisk/ping";
const REQUEST_BUDGET: Duration = Duration::from_secs(35);
const OLD: Access = Access {
    user: "writer",
    password: "running-password",
    origin: "https://running.example",
};
const REPLACEMENT: Access = Access {
    user: "writer",
    password: "replacement-password",
    origin: "https://replacement.example",
};
const READER: Access = Access {
    user: "reader",
    password: "reader-password",
    origin: OLD.origin,
};

type Events = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy)]
struct Access {
    user: &'static str,
    password: &'static str,
    origin: &'static str,
}

enum Source {
    Directory,
    Embedded,
}

enum Observation {
    Entered,
    Destroyed,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn verify(condition: bool, message: &'static str) -> Result<(), ControlError> {
    condition
        .then_some(())
        .ok_or(ControlError::Invalid(message))
}

fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; configuration reload cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(20), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            let primary = Err(failure(primary));
            match task.await {
                Ok(cleanup) => combine(primary, cleanup),
                Err(error) if error.is_cancelled() => primary,
                Err(error) => combine(primary, Err(failure(error))),
            }
        }
    }
}

fn write_configuration(
    directory: &Path,
    http: SocketAddr,
    sip: SocketAddr,
    rtp: (u16, u16),
    access: Access,
) -> Result<(), ControlError> {
    for (file, text) in [
        (
            "http.conf",
            format!("[general]\nbindaddr={}\nbindport={}\n", http.ip(), http.port()),
        ),
        (
            "ari.conf",
            format!("[general]\nallowed_origins={}\n[writer]\ntype=user\npassword={}\n[reader]\ntype=user\npassword=reader-password\nread_only=yes\n", access.origin, access.password),
        ),
        (
            "pjsip.conf",
            format!("[udp]\ntype=transport\nprotocol=udp\nbind={sip}\n"),
        ),
        (
            "rtp.conf",
            format!("[general]\nrtpstart={}\nrtpend={}\n", rtp.0, rtp.1),
        ),
        ("extensions.conf", String::new()),
        ("chan_websocket.conf", String::new()),
        ("websocket_client.conf", String::new()),
        ("musiconhold.conf", String::new()),
    ] {
        std::fs::write(directory.join(file), text).map_err(failure)?;
    }
    Ok(())
}

fn reserve_media() -> Result<[std::net::UdpSocket; 4], ControlError> {
    for _ in 0..64 {
        let first = std::net::UdpSocket::bind("127.0.0.1:0").map_err(failure)?;
        let local = first.local_addr().map_err(failure)?;
        let start = local.port();
        if start % 2 != 0 || start.checked_add(3).is_none() {
            continue;
        }
        let remaining = (|| {
            Ok::<_, io::Error>((
                std::net::UdpSocket::bind((local.ip(), start + 1))?,
                std::net::UdpSocket::bind((local.ip(), start + 2))?,
                std::net::UdpSocket::bind((local.ip(), start + 3))?,
            ))
        })();
        match remaining {
            Ok((second, third, fourth)) => return Ok([first, second, third, fourth]),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(failure(error)),
        }
    }
    Err(ControlError::Capacity)
}

fn authorization(access: Access) -> Result<HeaderValue, ControlError> {
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", access.user, access.password));
    let mut value = HeaderValue::from_str(&format!("Basic {encoded}")).map_err(failure)?;
    value.set_sensitive(true);
    Ok(value)
}

struct Reply {
    status: StatusCode,
    bytes: Vec<u8>,
}

impl Reply {
    fn status(self, expected: StatusCode) -> Result<Vec<u8>, ControlError> {
        if self.status != expected {
            return Err(failure(format!(
                "Configuration request returned {}; expected {expected}",
                self.status
            )));
        }
        Ok(self.bytes)
    }
}

struct HttpConnection {
    address: SocketAddr,
    sender: SendRequest<Full<Bytes>>,
    connection: JoinHandle<Result<(), ControlError>>,
}

impl HttpConnection {
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

    async fn request(
        &mut self,
        method: Method,
        path: &str,
        access: Option<Access>,
        body: &str,
    ) -> Result<Reply, ControlError> {
        tokio::time::timeout(REQUEST_BUDGET, async {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("Host", self.address.to_string());
            if let Some(access) = access {
                request = request
                    .header("Authorization", authorization(access)?)
                    .header("Origin", access.origin);
            }
            let request = request
                .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                .map_err(failure)?;
            let response = self.sender.send_request(request).await.map_err(failure)?;
            let status = response.status();
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Some(data) = frame.map_err(failure)?.data_ref() {
                    let length = bytes
                        .len()
                        .checked_add(data.len())
                        .ok_or(ControlError::Capacity)?;
                    if length > 16_384 {
                        return Err(ControlError::Capacity);
                    }
                    bytes.extend_from_slice(data);
                }
            }
            Ok(Reply { status, bytes })
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
                Some(Message::Text(_) | Message::Ping(_) | Message::Pong(_)) => {}
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
    directory: tempfile::TempDir,
    config_directory: std::path::PathBuf,
    data: std::path::PathBuf,
    http: SocketAddr,
    sip: SocketAddr,
    controller: Controller,
    server: JoinHandle<Result<(), ControlError>>,
    connection: Option<HttpConnection>,
    events: Option<Events>,
    peer: tokio::net::UdpSocket,
    media_reservations: [Option<(std::net::UdpSocket, std::net::UdpSocket)>; 2],
    media_addresses: [SocketAddr; 4],
    rtp: (u16, u16),
}

impl Fixture {
    async fn new(source: Source) -> Result<Self, ControlError> {
        let directory = tempfile::tempdir().map_err(failure)?;
        let initialized = async {
            let config_directory = directory.path().join("startup settings");
            let data = directory.path().join("owned data");
            std::fs::create_dir(&config_directory).map_err(failure)?;
            let http_reservation = std::net::TcpListener::bind("127.0.0.1:0").map_err(failure)?;
            let sip_reservation = std::net::UdpSocket::bind("127.0.0.1:0").map_err(failure)?;
            let http = http_reservation.local_addr().map_err(failure)?;
            let sip = sip_reservation.local_addr().map_err(failure)?;
            let peer = tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .map_err(failure)?;
            let media_reservations = reserve_media()?;
            let media_addresses = [
                media_reservations[0].local_addr().map_err(failure)?,
                media_reservations[1].local_addr().map_err(failure)?,
                media_reservations[2].local_addr().map_err(failure)?,
                media_reservations[3].local_addr().map_err(failure)?,
            ];
            let rtp = (media_addresses[0].port(), media_addresses[3].port());
            write_configuration(&config_directory, http, sip, rtp, OLD)?;
            let embedded = match source {
                Source::Directory => None,
                Source::Embedded => Some(Config::load(&config_directory).map_err(failure)?),
            };
            drop(http_reservation);
            drop(sip_reservation);
            let server = match embedded {
                Some(config) => Server::bind(config, 4, data.clone()).await?,
                None => {
                    Server::bind_from_directory(config_directory.clone(), 4, data.clone()).await?
                }
            };
            let controller = server.controller();
            let server = tokio::spawn(server.run());
            Ok::<_, ControlError>((
                config_directory,
                data,
                http,
                sip,
                peer,
                controller,
                server,
                media_reservations,
                media_addresses,
                rtp,
            ))
        }
        .await;
        let (
            config_directory,
            data,
            http,
            sip,
            peer,
            controller,
            server,
            media_reservations,
            media_addresses,
            rtp,
        ) = match initialized {
            Ok(initialized) => initialized,
            Err(primary) => return combine(Err(primary), directory.close().map_err(failure)),
        };
        let [first, second, third, fourth] = media_reservations;
        Ok(Self {
            directory,
            config_directory,
            data,
            http,
            sip,
            controller,
            server,
            connection: None,
            events: None,
            peer,
            media_reservations: [Some((first, second)), Some((third, fourth))],
            media_addresses,
            rtp,
        })
    }

    async fn connect(&mut self) -> Result<(), ControlError> {
        if self.connection.is_some() {
            return Err(ControlError::Conflict("HTTP connection already owned"));
        }
        self.connection = Some(HttpConnection::connect(self.http).await?);
        Ok(())
    }

    async fn request(
        &mut self,
        method: Method,
        path: &str,
        access: Option<Access>,
        body: &str,
    ) -> Result<Reply, ControlError> {
        self.connection
            .as_mut()
            .ok_or(ControlError::Invalid("HTTP connection missing"))?
            .request(method, path, access, body)
            .await
    }

    fn replace_files(&self, access: Access) -> Result<(), ControlError> {
        write_configuration(
            &self.config_directory,
            self.http,
            self.sip,
            self.rtp,
            access,
        )
    }

    async fn subscribe(&mut self) -> Result<(), ControlError> {
        let mut request = format!("ws://{}/ari/events?app=reload", self.http)
            .into_client_request()
            .map_err(failure)?;
        request
            .headers_mut()
            .insert("Authorization", authorization(OLD)?);
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_static(OLD.origin));
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("ari"));
        let (socket, response) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(failure)?
        .map_err(failure)?;
        self.events = Some(socket);
        verify(
            response.status() == StatusCode::SWITCHING_PROTOCOLS
                && response.headers().get("Sec-WebSocket-Protocol")
                    == Some(&HeaderValue::from_static("ari")),
            "Event upgrade changed its negotiated protocol",
        )
    }

    async fn observed(&mut self, id: &str, observation: Observation) -> Result<(), ControlError> {
        let events = self.events.as_mut().ok_or(ControlError::Capacity)?;
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..64 {
                match events.next().await.transpose().map_err(failure)? {
                    Some(Message::Text(text)) => {
                        let event: Event = serde_json::from_str(&text)?;
                        let matches = match observation {
                            Observation::Entered => matches!(event.kind, EventKind::StasisStart { channel, .. } if channel.id.as_str() == id),
                            Observation::Destroyed => matches!(event.kind, EventKind::ChannelDestroyed { channel, .. } if channel.id.as_str() == id),
                        };
                        if matches {
                            return verify(event.application == "reload", "Event moved to another application");
                        }
                    }
                    Some(Message::Ping(bytes)) => events.send(Message::Pong(bytes)).await.map_err(failure)?,
                    Some(Message::Pong(_)) => {}
                    Some(Message::Close(_) | Message::Binary(_) | Message::Frame(_)) | None => {
                        return Err(ControlError::Invalid("Established event connection ended during reload"));
                    }
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)?
    }

    async fn channel(&mut self, id: &str, access: Access) -> Result<Channel, ControlError> {
        let peer = self.peer.local_addr().map_err(failure)?;
        let (data, control) = self
            .media_reservations
            .iter_mut()
            .find_map(Option::take)
            .ok_or(ControlError::Capacity)?;
        let expected = data.local_addr().map_err(failure)?;
        drop((data, control));
        let bytes = self.request(Method::POST,
            &format!("/ari/channels/externalMedia?app=reload&channelId={id}&format=ulaw&external_host={peer}"),
            Some(access), "").await?.status(StatusCode::OK)?;
        let channel: Channel = serde_json::from_slice(&bytes)?;
        verify(
            channel.id.as_str() == id,
            "External channel changed its identity",
        )?;
        let variables = channel.channelvars.as_ref().ok_or(ControlError::Capacity)?;
        let address = variables
            .get("UNICASTRTP_LOCAL_ADDRESS")
            .ok_or(ControlError::Capacity)?
            .parse()
            .map_err(failure)?;
        let port: u16 = variables
            .get("UNICASTRTP_LOCAL_PORT")
            .ok_or(ControlError::Capacity)?
            .parse()
            .map_err(failure)?;
        verify(
            SocketAddr::new(address, port) == expected,
            "External media did not use its owned reserved port pair",
        )?;
        self.observed(id, Observation::Entered).await?;
        self.snapshot(id, access).await
    }

    async fn snapshot(&mut self, id: &str, access: Access) -> Result<Channel, ControlError> {
        let bytes = self
            .request(
                Method::GET,
                &format!("/ari/channels/{id}"),
                Some(access),
                "",
            )
            .await?
            .status(StatusCode::OK)?;
        serde_json::from_slice(&bytes).map_err(Into::into)
    }

    async fn finish(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let Self {
            directory,
            data,
            http,
            sip,
            controller,
            server,
            connection,
            mut events,
            peer,
            media_reservations,
            media_addresses,
            ..
        } = self;
        let peer_address = peer.local_addr().map_err(failure);
        let mut result = primary;
        if let Some(events) = &mut events {
            result = combine(result, close_events(events).await);
        }
        drop(events);
        if let Some(connection) = connection {
            result = combine(result, connection.close().await);
        }
        controller.shutdown();
        result = combine(result, joined(server).await);
        drop(peer);
        drop(media_reservations);
        result = combine(
            result,
            std::net::TcpListener::bind(http).map(drop).map_err(failure),
        );
        for address in [Ok(sip), peer_address]
            .into_iter()
            .chain(media_addresses.into_iter().map(Ok))
        {
            let rebound = address.and_then(|address| {
                std::net::UdpSocket::bind(address)
                    .map(drop)
                    .map_err(failure)
            });
            result = combine(result, rebound);
        }
        result = combine(
            result,
            rusterisk_storage::Store::open(&data)
                .map(drop)
                .map_err(failure),
        );
        combine(result, directory.close().map_err(failure))
    }
}

#[tokio::test]
async fn embedded_servers_report_an_unavailable_directory_reload_source() -> Result<(), ControlError>
{
    let mut fixture = Fixture::new(Source::Embedded).await?;
    let primary = async {
        fixture.connect().await?;
        fixture
            .request(Method::POST, RELOAD, Some(OLD), "")
            .await?
            .status(StatusCode::NOT_IMPLEMENTED)?;
        fixture
            .request(Method::GET, PING, Some(OLD), "")
            .await?
            .status(StatusCode::OK)?;
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn directory_reload_checks_current_access_and_preserves_connections_and_calls(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new(Source::Directory).await?;
    let primary = async {
        fixture.connect().await?;
        for (access, path, body, expected) in [
            (None, RELOAD, "", StatusCode::UNAUTHORIZED),
            (Some(READER), RELOAD, "", StatusCode::FORBIDDEN),
            (
                Some(Access {
                    origin: "https://unlisted.example",
                    ..OLD
                }),
                RELOAD,
                "",
                StatusCode::FORBIDDEN,
            ),
            (Some(OLD), RELOAD, "{}", StatusCode::BAD_REQUEST),
            (
                Some(OLD),
                "/rusterisk/configuration/reload?directory=other",
                "",
                StatusCode::BAD_REQUEST,
            ),
        ] {
            fixture
                .request(Method::POST, path, access, body)
                .await?
                .status(expected)?;
        }
        fixture.subscribe().await?;
        let original = fixture.channel("before-reload", OLD).await?;
        fixture.replace_files(REPLACEMENT)?;
        let bytes = fixture
            .request(Method::POST, RELOAD, Some(OLD), "")
            .await?
            .status(StatusCode::NO_CONTENT)?;
        verify(bytes.is_empty(), "Reload 204 carried response content")?;
        fixture
            .request(Method::GET, PING, Some(REPLACEMENT), "")
            .await?
            .status(StatusCode::OK)?;
        fixture
            .request(
                Method::GET,
                PING,
                Some(Access {
                    password: OLD.password,
                    ..REPLACEMENT
                }),
                "",
            )
            .await?
            .status(StatusCode::UNAUTHORIZED)?;
        fixture
            .request(
                Method::GET,
                PING,
                Some(Access {
                    origin: OLD.origin,
                    ..REPLACEMENT
                }),
                "",
            )
            .await?
            .status(StatusCode::FORBIDDEN)?;
        verify(
            fixture.snapshot("before-reload", REPLACEMENT).await? == original,
            "Reload changed an existing channel snapshot",
        )?;
        fixture.channel("after-reload", REPLACEMENT).await?;
        fixture
            .request(
                Method::DELETE,
                "/ari/channels/before-reload",
                Some(REPLACEMENT),
                "",
            )
            .await?
            .status(StatusCode::NO_CONTENT)?;
        fixture
            .observed("before-reload", Observation::Destroyed)
            .await
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn invalid_directory_settings_report_their_position_and_preserve_running_state(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new(Source::Directory).await?;
    let primary = async {
        fixture.connect().await?;
        fixture.subscribe().await?;
        let original = fixture.channel("invalid-replacement", OLD).await?;
        fixture.replace_files(REPLACEMENT)?;
        std::fs::write(
            fixture.config_directory.join("http.conf"),
            format!(
                "[general]\nbindaddr={}\nbindport={}\nunexpected=yes\n",
                fixture.http.ip(),
                fixture.http.port()
            ),
        )
        .map_err(failure)?;
        let rejected = fixture.request(Method::POST, RELOAD, Some(OLD), "").await?;
        verify(
            matches!(
                rejected.status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "Invalid replacement did not return a configuration rejection",
        )?;
        let error: serde_json::Value = serde_json::from_slice(&rejected.bytes)?;
        verify(
            error["file"] == "http.conf"
                && error["line"] == 4
                && error["reason"] == "unsupported option unexpected",
            "Configuration rejection lost its file, line, or reason",
        )?;
        fixture
            .request(Method::GET, PING, Some(OLD), "")
            .await?
            .status(StatusCode::OK)?;
        fixture
            .request(
                Method::GET,
                PING,
                Some(Access {
                    password: REPLACEMENT.password,
                    ..OLD
                }),
                "",
            )
            .await?
            .status(StatusCode::UNAUTHORIZED)?;
        verify(
            fixture.snapshot("invalid-replacement", OLD).await? == original,
            "Rejected settings changed an existing channel",
        )?;
        fixture
            .request(
                Method::DELETE,
                "/ari/channels/invalid-replacement",
                Some(OLD),
                "",
            )
            .await?
            .status(StatusCode::NO_CONTENT)?;
        fixture
            .observed("invalid-replacement", Observation::Destroyed)
            .await
    }
    .await;
    fixture.finish(primary).await
}
