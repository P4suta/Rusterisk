use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use rusterisk_client::{api::ApiError, Error};
use rusterisk_media::{mulaw_decode, mulaw_encode};
use rusterisk_server::test_time::Wait;
use serde::Deserialize;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig, Message,
};

type Peer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn invalid() -> Error {
    Error::Invalid("home-telephone media comparison")
}

fn phase<T>(result: Result<T, Error>, name: &'static str) -> Result<T, Error> {
    if result.is_err() {
        eprintln!("Home-telephone media comparison failed: phase={name}");
    }
    result
}

fn combine(primary: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(Error::Cleanup(format!(
            "{primary}; home-telephone cleanup failed: {cleanup}"
        ))),
    }
}

struct Gateway {
    base: url::Url,
    http: reqwest::Client,
}

impl Gateway {
    fn new(address: &str) -> Result<Self, Error> {
        Ok(Self {
            base: url::Url::parse(address).map_err(|_| invalid())?,
            http: reqwest::Client::builder()
                .timeout(Wait::Ready.duration())
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    fn resource(&self, path: &[&str], query: &[(&str, &str)]) -> Result<url::Url, Error> {
        let mut url = self.base.clone();
        let mut segments = url.path_segments_mut().map_err(|()| invalid())?;
        segments.clear().push("ari");
        for segment in path {
            segments.push(segment);
        }
        drop(segments);
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        Ok(url)
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<reqwest::Response, Error> {
        Ok(self
            .http
            .request(method, self.resource(path, query)?)
            .basic_auth("app", Some("test-password"))
            .send()
            .await?)
    }

    async fn command(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<(), Error> {
        let deleting = method == Method::DELETE;
        let response = self.request(method, path, query).await?;
        let status = response.status();
        if status.is_success() || deleting && status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Error::Api {
                status: status.as_u16(),
                message: ApiError {
                    message: "Home-telephone command failed".into(),
                },
            })
        }
    }

    async fn variable(&self, channel: &str) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Variable {
            value: String,
        }
        let mut response = self
            .request(
                Method::GET,
                &["channels", channel, "variable"],
                &[("variable", "MEDIA_WEBSOCKET_CONNECTION_ID")],
            )
            .await?
            .error_for_status()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err(Error::Capacity);
            }
            body.extend_from_slice(&chunk);
        }
        let variable: Variable = serde_json::from_slice(&body)?;
        uuid::Uuid::parse_str(&variable.value).map_err(|_| invalid())?;
        Ok(variable.value)
    }

    async fn socket(&self, path: &str, application: Option<&str>) -> Result<Peer, Error> {
        let mut url = self.base.clone();
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => return Err(invalid()),
        };
        url.set_scheme(scheme).map_err(|()| invalid())?;
        url.set_path(path);
        if let Some(application) = application {
            url.query_pairs_mut().append_pair("app", application);
        }
        let mut request = url.as_str().into_client_request()?;
        let mut authorization = HeaderValue::from_str(&format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("app:test-password")
        ))
        .map_err(|_| invalid())?;
        authorization.set_sensitive(true);
        request.headers_mut().insert("Authorization", authorization);
        if request.headers().contains_key("Sec-WebSocket-Protocol") {
            return Err(invalid());
        }
        let (peer, response) = Wait::Ready
            .bounded(tokio_tungstenite::connect_async_with_config(
                request,
                Some(
                    WebSocketConfig::default()
                        .max_message_size(Some(512 * 1024))
                        .max_frame_size(Some(512 * 1024)),
                ),
                false,
            ))
            .await
            .map_err(|_| Error::Timeout)??;
        if response.headers().contains_key("Sec-WebSocket-Protocol") {
            return Err(invalid());
        }
        Ok(peer)
    }
}

#[derive(Deserialize)]
struct Channel {
    id: String,
    state: String,
    creationtime: String,
}

impl Channel {
    fn matches(&self, expected: &str) -> bool {
        self.id == expected
            && !self.state.is_empty()
            && !self.creationtime.is_empty()
            && self.creationtime.len() <= 64
            && self.creationtime.is_ascii()
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    StasisStart {
        channel: Channel,
        args: Vec<String>,
    },
    StasisEnd {
        channel: Channel,
    },
    ChannelDestroyed {
        channel: Channel,
    },
    #[serde(other)]
    Other,
}

async fn event(peer: &mut Peer) -> Result<Event, Error> {
    match peer.next().await.ok_or(Error::Closed)?? {
        Message::Text(text) => Ok(serde_json::from_str(&text)?),
        Message::Ping(bytes) => {
            peer.send(Message::Pong(bytes)).await?;
            Ok(Event::Other)
        }
        Message::Pong(_) => Ok(Event::Other),
        Message::Binary(_) | Message::Close(_) | Message::Frame(_) => Err(Error::Closed),
    }
}

async fn stasis(peer: &mut Peer, expected: &str) -> Result<(), Error> {
    Wait::Ready
        .bounded(async {
            for _ in 0..128 {
                match event(peer).await? {
                    Event::StasisStart { channel, args }
                        if channel.matches(expected) && args.is_empty() =>
                    {
                        return Ok(())
                    }
                    Event::StasisStart { .. }
                    | Event::StasisEnd { .. }
                    | Event::ChannelDestroyed { .. }
                    | Event::Other => {}
                }
            }
            Err(Error::Capacity)
        })
        .await
        .map_err(|_| Error::Timeout)?
}

async fn retired(
    peer: &mut Peer,
    channels: &[String; 2],
    expected: [bool; 2],
) -> Result<(), Error> {
    Wait::Ready
        .bounded(async {
            let mut ended = [false; 2];
            for _ in 0..128 {
                if expected
                    .iter()
                    .enumerate()
                    .all(|(index, required)| !required || ended[index])
                {
                    return Ok(());
                }
                if let Event::StasisEnd { channel } | Event::ChannelDestroyed { channel } =
                    event(peer).await?
                {
                    for (index, id) in channels.iter().enumerate() {
                        if channel.matches(id) {
                            ended[index] = true;
                        }
                    }
                }
            }
            Err(Error::Capacity)
        })
        .await
        .map_err(|_| Error::Timeout)?
}

async fn media_started(peer: &mut Peer, channel: &str, connection: &str) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Start {
        event: String,
        channel_id: String,
        connection_id: String,
        format: String,
        optimal_frame_size: u16,
        ptime: u16,
    }
    let message = Wait::Ready
        .bounded(peer.next())
        .await
        .map_err(|_| Error::Timeout)?
        .ok_or(Error::Closed)??;
    let Message::Text(text) = message else {
        return Err(invalid());
    };
    let start: Start = serde_json::from_str(&text)?;
    if start.event != "MEDIA_START"
        || start.channel_id != channel
        || start.connection_id != connection
        || start.format != "ulaw"
        || start.optimal_frame_size != 160
        || start.ptime != 20
    {
        return Err(invalid());
    }
    Ok(())
}

fn received(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    expected: i32,
) -> Result<bool, Error> {
    match message.ok_or(Error::Closed)?? {
        Message::Binary(bytes) => Ok(bytes.len() == 160
            && bytes
                .iter()
                .all(|byte| (i32::from(mulaw_decode(*byte)) - expected).abs() < 300)),
        Message::Text(_) | Message::Ping(_) | Message::Pong(_) => Ok(false),
        Message::Close(_) | Message::Frame(_) => Err(Error::Closed),
    }
}

async fn audio(left: &mut Peer, right: &mut Peer) -> Result<(), Error> {
    Wait::Ready
        .bounded(async {
            let mut clock = tokio::time::interval(Duration::from_millis(20));
            clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut sent = 0;
            let mut known = [0; 2];
            loop {
                tokio::select! {
                    _instant = clock.tick(), if sent < 150 => {
                        left.send(Message::Binary(vec![mulaw_encode(4000); 160].into())).await?;
                        right.send(Message::Binary(vec![mulaw_encode(-7000); 160].into())).await?;
                        sent += 1;
                    }
                    message = left.next() => {
                        known[0] = if received(message, -7000)? { known[0] + 1 } else { 0 };
                    }
                    message = right.next() => {
                        known[1] = if received(message, 4000)? { known[1] + 1 } else { 0 };
                    }
                }
                if sent >= 30 && known.iter().all(|count| *count >= 4) {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
}

#[tokio::test]
async fn gateway_keeps_sending_until_both_delayed_audio_paths_are_observed() -> Result<(), Error> {
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{tungstenite::protocol::Role, MaybeTlsStream, WebSocketStream};

    async fn pair() -> Result<(Peer, Peer), Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let (client, (server, _)) = tokio::try_join!(
            TcpStream::connect(listener.local_addr()?),
            listener.accept()
        )?;
        Ok(tokio::join!(
            WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client), Role::Client, None),
            WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(server), Role::Server, None)
        ))
    }
    let (mut left, mut first) = pair().await?;
    let (mut right, mut second) = pair().await?;
    let bridge = async {
        for frame in 0..44 {
            let (a, b) = tokio::join!(first.next(), second.next());
            let a = a.ok_or(Error::Closed)??;
            let b = b.ok_or(Error::Closed)??;
            if frame >= 36 {
                first.send(b).await?;
                if frame == 39 {
                    second.send(Message::Binary(vec![0xff; 160].into())).await?;
                } else {
                    second.send(a).await?;
                }
            }
        }
        Ok::<(), Error>(())
    };
    Wait::Ready
        .bounded(async { tokio::try_join!(audio(&mut left, &mut right), bridge) })
        .await
        .map_err(|_| Error::Timeout)??;
    Ok(())
}

async fn close(peer: &mut Peer) -> Result<(), Error> {
    Wait::Ready
        .bounded(async {
            peer.close(None).await?;
            for _ in 0..128 {
                match peer.next().await.transpose()? {
                    None | Some(Message::Close(_)) => return Ok(()),
                    Some(Message::Text(_) | Message::Binary(_) | Message::Pong(_)) => {}
                    Some(Message::Ping(bytes)) => peer.send(Message::Pong(bytes)).await?,
                    Some(Message::Frame(_)) => return Err(invalid()),
                }
            }
            Err(Error::Capacity)
        })
        .await
        .map_err(|_| Error::Timeout)?
}

struct Fixture {
    gateway: Gateway,
    application: String,
    bridge: String,
    channels: [String; 2],
    events: Peer,
    media: [Option<Peer>; 2],
    started: [bool; 2],
}

impl Fixture {
    async fn new(address: &str) -> Result<Self, Error> {
        let gateway = Gateway::new(address)?;
        let call = uuid::Uuid::new_v4();
        let application = format!("home-telephone-media-{call}");
        let events = phase(
            gateway.socket("/ari/events", Some(&application)).await,
            "event upgrade",
        )?;
        Ok(Self {
            gateway,
            application,
            bridge: format!("hp-{call}"),
            channels: [
                format!("hp-{call}-media-{}", uuid::Uuid::new_v4()),
                format!("hp-{call}-media-{}", uuid::Uuid::new_v4()),
            ],
            events,
            media: [None, None],
            started: [false; 2],
        })
    }

    async fn run(&mut self) -> Result<(), Error> {
        self.gateway
            .command(
                Method::POST,
                &["bridges", &self.bridge],
                &[("type", "mixing,proxy_media")],
            )
            .await?;
        for index in 0..2 {
            let channel = &self.channels[index];
            self.gateway
                .command(
                    Method::POST,
                    &["channels", "externalMedia"],
                    &[
                        ("app", &self.application),
                        ("external_host", "INCOMING"),
                        ("transport", "websocket"),
                        ("encapsulation", "none"),
                        ("connection_type", "server"),
                        ("format", "ulaw"),
                        ("direction", "both"),
                        ("channelId", channel),
                        ("transport_data", "f(json)"),
                    ],
                )
                .await?;
            let connection = phase(self.gateway.variable(channel).await, "media variable")?;
            self.media[index] = Some(phase(
                self.gateway
                    .socket(&format!("/media/{connection}"), None)
                    .await,
                "media upgrade",
            )?);
            phase(
                media_started(
                    self.media[index].as_mut().ok_or(Error::Closed)?,
                    channel,
                    &connection,
                )
                .await,
                "media start",
            )?;
            phase(stasis(&mut self.events, channel).await, "media Stasis")?;
            self.started[index] = true;
            self.gateway
                .command(
                    Method::POST,
                    &["bridges", &self.bridge, "addChannel"],
                    &[("channel", channel)],
                )
                .await?;
        }
        let [left, right] = &mut self.media;
        phase(
            audio(
                left.as_mut().ok_or(Error::Closed)?,
                right.as_mut().ok_or(Error::Closed)?,
            )
            .await,
            "received audio",
        )
    }

    async fn cleanup(&mut self) -> Result<(), Error> {
        let mut result = Ok(());
        for peer in self.media.iter_mut().flatten() {
            result = combine(result, phase(close(peer).await, "media close"));
        }
        for channel in &self.channels {
            result = combine(
                result,
                self.gateway
                    .command(Method::DELETE, &["channels", channel], &[])
                    .await,
            );
        }
        result = combine(
            result,
            phase(
                retired(&mut self.events, &self.channels, self.started).await,
                "channel retirement",
            ),
        );
        result = combine(
            result,
            self.gateway
                .command(Method::DELETE, &["bridges", &self.bridge], &[])
                .await,
        );
        combine(result, phase(close(&mut self.events).await, "event close"))
    }
}

pub async fn compare(address: &str) -> Result<(), Error> {
    let mut fixture = Fixture::new(address).await?;
    let result = phase(
        Wait::Scenario
            .bounded(fixture.run())
            .await
            .map_err(|_| Error::Timeout),
        "call deadline",
    )
    .and_then(|result| result);
    combine(result, fixture.cleanup().await)?;
    println!("Home-telephone media and cleanup passed for {address}.");
    Ok(())
}
