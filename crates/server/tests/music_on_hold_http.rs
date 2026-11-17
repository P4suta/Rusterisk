use futures_util::StreamExt;
use reqwest::Method;
use rusterisk_api::{Channel, Event, EventKind};
use rusterisk_config::Config;
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::{rtp::SocketPair, runtime::Server, ControlError};
use rusterisk_storage::Recorder;
use std::{net::SocketAddr, path::Path, time::Duration};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

#[path = "music_on_hold_http/hold.rs"]
mod hold;

type Events =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; music test cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined<E: std::fmt::Display>(
    mut task: tokio::task::JoinHandle<Result<(), E>>,
) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            let result = Err(failure(primary));
            match task.await {
                Err(error) if error.is_cancelled() => result,
                Err(error) => combine(result, Err(failure(error))),
                Ok(Err(error)) => combine(result, Err(failure(error))),
                Ok(Ok(())) => result,
            }
        }
    }
}

fn configuration(root: &Path) -> Result<Config, ControlError> {
    for (name, text) in [
        ("http.conf", "[general]\nbindaddr=127.0.0.1\n"),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        ("pjsip.conf", ""),
        ("rtp.conf", ""),
        ("extensions.conf", ""),
        (
            "musiconhold.conf",
            "[default]\nmode=files\ndirectory=moh\nsort=alpha\n",
        ),
    ] {
        std::fs::write(root.join(name), text).map_err(failure)?;
    }
    Config::load(root).map_err(failure)
}

fn music(root: &Path, frame: Frame, repetitions: usize) -> Result<(), ControlError> {
    let mut recorder = Recorder::create(root, "music-fixture", 8000).map_err(failure)?;
    for _ in 0..repetitions {
        recorder.write(frame).map_err(failure)?;
    }
    recorder.finish().map_err(failure)?;
    let directory = root.join("moh");
    std::fs::create_dir(&directory).map_err(failure)?;
    std::fs::rename(
        root.join("recordings/music-fixture.wav"),
        directory.join("known.wav"),
    )
    .map_err(failure)
}

async fn request(
    http: &reqwest::Client,
    address: SocketAddr,
    method: Method,
    path: &str,
    query: &[(&str, &str)],
) -> Result<reqwest::Response, ControlError> {
    let mut url = reqwest::Url::parse(&format!("http://{address}/ari/{path}")).map_err(failure)?;
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    http.request(method, url)
        .basic_auth("app", Some("test-password"))
        .send()
        .await
        .map_err(failure)
}

async fn command(
    http: &reqwest::Client,
    address: SocketAddr,
    method: Method,
    path: &str,
    query: &[(&str, &str)],
) -> Result<(), ControlError> {
    let deleting = method == Method::DELETE;
    let status = request(http, address, method, path, query).await?.status();
    if status.is_success() || deleting && status == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(failure(format!(
            "ARI command returned HTTP {}",
            status.as_u16()
        )))
    }
}

async fn started(events: &mut Events, expected: &str) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..64 {
            let message = events.next().await.ok_or(ControlError::Capacity)?.map_err(failure)?;
            let event: Event = serde_json::from_str(message.to_text().map_err(failure)?)?;
            if matches!(event.kind, EventKind::StasisStart { channel, .. } if channel.id.as_str() == expected) {
                return Ok(());
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

async fn closed(events: &mut Events) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        events.close(None).await.map_err(failure)?;
        for _ in 0..64 {
            match events.next().await.transpose().map_err(failure)? {
                Some(Message::Close(None)) => {
                    if events.next().await.transpose().map_err(failure)?.is_some() {
                        return Err(ControlError::Invalid("Event TCP connection remained open"));
                    }
                    return Ok(());
                }
                Some(Message::Text(_)) => {}
                Some(
                    Message::Binary(_)
                    | Message::Ping(_)
                    | Message::Pong(_)
                    | Message::Close(Some(_))
                    | Message::Frame(_),
                )
                | None => {
                    return Err(ControlError::Invalid("Event close response missing"));
                }
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

struct Peer {
    socket: SocketPair,
    destination: Option<SocketAddr>,
    sender: RtpSender,
}

impl Peer {
    fn new(seed: u8) -> Result<Self, ControlError> {
        Ok(Self {
            socket: SocketPair::bind("127.0.0.1".parse().map_err(failure)?, 0, 0)
                .map_err(failure)?,
            destination: None,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [seed; 10]).map_err(failure)?,
        })
    }

    async fn create(
        &mut self,
        http: &reqwest::Client,
        address: SocketAddr,
        channel: &str,
    ) -> Result<(), ControlError> {
        let remote = self.socket.local_addr().map_err(failure)?.to_string();
        let response = request(
            http,
            address,
            Method::POST,
            "channels/externalMedia",
            &[
                ("app", "music"),
                ("external_host", &remote),
                ("format", "ulaw"),
                ("channelId", channel),
            ],
        )
        .await?
        .error_for_status()
        .map_err(failure)?;
        let snapshot: Channel = response.json().await.map_err(failure)?;
        if snapshot.id.as_str() != channel {
            return Err(ControlError::Invalid("External channel identity changed"));
        }
        let variables = snapshot.channelvars.ok_or(ControlError::Capacity)?;
        let ip = variables
            .get("UNICASTRTP_LOCAL_ADDRESS")
            .ok_or(ControlError::Capacity)?;
        let port = variables
            .get("UNICASTRTP_LOCAL_PORT")
            .ok_or(ControlError::Capacity)?;
        self.destination = Some(format!("{ip}:{port}").parse().map_err(failure)?);
        Ok(())
    }

    async fn send(&mut self, sample: i16) -> Result<(), ControlError> {
        let packet = self
            .sender
            .packet(Frame::new(&[sample; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
        let bytes = packet.encode().map_err(failure)?;
        let sent = self
            .socket
            .data()
            .send_to(&bytes, self.destination.ok_or(ControlError::Capacity)?)
            .await
            .map_err(failure)?;
        if sent != bytes.len() {
            return Err(ControlError::Invalid("Incomplete audio datagram"));
        }
        Ok(())
    }
}

fn known(bytes: &[u8], expected: i32) -> Result<bool, ControlError> {
    let packet = RtpPacket::parse(bytes).map_err(failure)?;
    Ok(packet.payload_type() == 0
        && packet.payload().len() == 160
        && packet
            .payload()
            .iter()
            .all(|byte| (i32::from(mulaw_decode(*byte)) - expected).abs() < 300))
}

async fn audio(
    left: &mut Peer,
    right: &mut Peer,
    sent_samples: [i16; 2],
    expected: [i32; 2],
    required: u8,
) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut consecutive = [0u8; 2];
        let mut first = [0; 2049];
        let mut second = [0; 2049];
        loop {
            tokio::select! {
                _instant = clock.tick(), if sent < 40 => {
                    left.send(sent_samples[0]).await?;
                    right.send(sent_samples[1]).await?;
                    sent += 1;
                }
                result = left.socket.data().recv_from(&mut first) => {
                    let (size, source) = result.map_err(failure)?;
                    if Some(source) != left.destination { return Err(ControlError::Invalid("Unexpected audio peer")); }
                    consecutive[0] = if known(&first[..size], expected[0])? { consecutive[0].saturating_add(1) } else { 0 };
                }
                result = right.socket.data().recv_from(&mut second) => {
                    let (size, source) = result.map_err(failure)?;
                    if Some(source) != right.destination { return Err(ControlError::Invalid("Unexpected audio peer")); }
                    consecutive[1] = if known(&second[..size], expected[1])? { consecutive[1].saturating_add(1) } else { 0 };
                }
            }
            if sent == 40 && consecutive.iter().all(|count| *count >= required) {
                return Ok(());
            }
        }
    })
    .await
    .map_err(failure)?
}

async fn run(root: &Path, frame: Frame, repetitions: usize) -> Result<(), ControlError> {
    let config_root = root.join("configuration");
    let data = root.join("data");
    std::fs::create_dir(&config_root).map_err(failure)?;
    std::fs::create_dir(&data).map_err(failure)?;
    let mut config = configuration(&config_root)?;
    config.http.bind.set_port(0);
    music(&data, frame, repetitions)?;
    let mut left = Peer::new(81)?;
    let mut right = Peer::new(82)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(failure)?;
    let server = Server::bind(config, 4, data).await?;
    let controller = server.controller();
    let address = server.local_addr();
    let server = tokio::spawn(server.run());
    let address = match address {
        Ok(address) => address,
        Err(error) => {
            controller.shutdown();
            return combine(Err(error), joined(server).await);
        }
    };
    let mut events = None;
    let mut phase = "event connection";
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut event_request = format!("ws://{address}/ari/events?app=music")
            .into_client_request()
            .map_err(failure)?;
        let mut authorization = HeaderValue::from_static("Basic YXBwOnRlc3QtcGFzc3dvcmQ=");
        authorization.set_sensitive(true);
        event_request
            .headers_mut()
            .insert("Authorization", authorization);
        events = Some(
            tokio_tungstenite::connect_async(event_request)
                .await
                .map_err(failure)?
                .0,
        );
        let events = events.as_mut().ok_or(ControlError::Capacity)?;
        phase = "external channels";
        left.create(&http, address, "root").await?;
        started(events, "root").await?;
        right.create(&http, address, "winner").await?;
        started(events, "winner").await?;
        command(
            &http,
            address,
            Method::POST,
            "bridges/music",
            &[("type", "mixing,proxy_media")],
        )
        .await?;
        for channel in ["root", "winner"] {
            command(
                &http,
                address,
                Method::POST,
                "bridges/music/addChannel",
                &[("channel", channel)],
            )
            .await?;
        }
        phase = "initial received audio";
        audio(&mut left, &mut right, [4000, -7000], [-7000, 4000], 4).await?;
        command(
            &http,
            address,
            Method::POST,
            "bridges/music/removeChannel",
            &[("channel", "winner")],
        )
        .await?;
        phase = "start music";
        let status = request(&http, address, Method::POST, "channels/root/moh", &[])
            .await?
            .status();
        if status != reqwest::StatusCode::NO_CONTENT {
            return Err(failure(format!(
                "Expected music HTTP 204, received {}",
                status.as_u16()
            )));
        }
        phase = "looped music received audio";
        audio(&mut left, &mut right, [4000, -7000], [1200, 0], 12).await?;
        command(
            &http,
            address,
            Method::POST,
            "bridges/music/addChannel",
            &[("channel", "winner")],
        )
        .await?;
        phase = "stop music";
        let status = request(&http, address, Method::DELETE, "channels/root/moh", &[])
            .await?
            .status();
        if status != reqwest::StatusCode::NO_CONTENT {
            return Err(failure(format!(
                "Expected stop music HTTP 204, received {}",
                status.as_u16()
            )));
        }
        phase = "resumed received audio";
        audio(&mut left, &mut right, [9000, -5000], [-5000, 9000], 4).await
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    if result.is_err() {
        eprintln!("Music-on-hold boundary failed: phase={phase}");
    }
    let mut result = result;
    for resource in ["channels/root", "channels/winner", "bridges/music"] {
        result = combine(
            result,
            command(&http, address, Method::DELETE, resource, &[]).await,
        );
    }
    if let Some(events) = &mut events {
        result = combine(result, closed(events).await);
    }
    drop(events);
    controller.shutdown();
    result = combine(result, joined(server).await);
    let rebound = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure)
        .map(drop);
    combine(result, rebound)
}

#[tokio::test]
async fn channel_music_loops_known_audio_and_stopping_restores_the_phone_bridge(
) -> Result<(), ControlError> {
    let frame = Frame::new(&[1200; 160], 8000).map_err(failure)?;
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = run(directory.path(), frame, 4).await;
    combine(result, directory.close().map_err(failure))
}

#[tokio::test]
async fn short_music_repeats_without_silence_inside_received_twenty_millisecond_audio(
) -> Result<(), ControlError> {
    let frame = Frame::new(&[1200; 40], 8000).map_err(failure)?;
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = run(directory.path(), frame, 1).await;
    combine(result, directory.close().map_err(failure))
}
