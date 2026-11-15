use super::external_websockets::{audio, controls, Peer};
use futures_util::{SinkExt, StreamExt};
use rusterisk_client::{
    api::{
        ChannelId, ChannelState, MediaCommand, MediaControlFormat, MediaEvent, MediaFormat,
        WebSocketMediaOptions,
    },
    Client, Error, Session,
};
use rusterisk_media::mulaw_encode;
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::{
    tungstenite::{
        handshake::server::{Callback, ErrorResponse, Request, Response},
        Message,
    },
    MaybeTlsStream,
};

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("Configured WebSocket comparison: {error}");
    Error::Invalid("configured WebSocket comparison")
}

struct MediaHandshake;

impl Callback for MediaHandshake {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        assert_eq!(request.uri().path(), "/agent");
        assert_eq!(request.uri().query(), Some("token=private-query"));
        assert_eq!(
            request.headers()["Authorization"],
            "Basic cGVlcjp0ZXN0LXBhc3N3b3Jk"
        );
        assert_eq!(request.headers()["Sec-WebSocket-Protocol"], "media");
        response.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("media"),
        );
        Ok(response)
    }
}

pub struct Fixture {
    port: u16,
    sockets: mpsc::Receiver<Peer>,
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), Error>>,
}

impl Fixture {
    pub async fn start() -> Result<Self, Error> {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
        let port = listener.local_addr()?.port();
        let (sender, sockets) = mpsc::channel(4);
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = tokio::select! {
                    _changed = stopped.changed() => return Ok(()),
                    accepted = listener.accept() => accepted?,
                };
                let handshake = tokio_tungstenite::accept_hdr_async(
                    MaybeTlsStream::Plain(stream),
                    MediaHandshake,
                );
                let socket = tokio::select! {
                    _changed = stopped.changed() => return Ok(()),
                    result = tokio::time::timeout(Duration::from_secs(2), handshake) => result.map_err(invalid)??,
                };
                sender.try_send(socket).map_err(|_| Error::Capacity)?;
            }
        });
        Ok(Self {
            port,
            sockets,
            stop,
            task,
        })
    }

    pub fn configuration(&self, host: &str) -> String {
        format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{host}:{}/agent?token=private-query\nprotocols=media\nusername=peer\npassword=test-password\nconnection_timeout=500\nreconnect_interval=500\nreconnect_attempts=4\n", self.port)
    }

    pub async fn configure_reference(&self, address: &str) -> Result<(), Error> {
        let configuration = std::env::var_os("RUSTERISK_REFERENCE_CONFIGURATION")
            .ok_or(Error::Invalid("reference configuration is required"))?;
        let configuration = std::path::PathBuf::from(configuration);
        tokio::fs::write(
            configuration.join("websocket_client.conf"),
            self.configuration("host.docker.internal"),
        )
        .await?;
        let response = reqwest::Client::new()
            .put(format!(
                "{address}/ari/asterisk/modules/res_websocket_client.so"
            ))
            .basic_auth("app", Some("test-password"))
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        Ok(())
    }

    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.stop.send_replace(true);
        match tokio::time::timeout(Duration::from_secs(3), &mut self.task).await {
            Ok(result) => result.map_err(invalid)?,
            Err(error) => {
                self.task.abort();
                if let Err(join) = self.task.await {
                    if !join.is_cancelled() {
                        return Err(invalid(join));
                    }
                }
                Err(invalid(error))
            }
        }
    }
}

async fn connect(
    client: &Client,
    session: &mut Session,
    app: &str,
    fixture: &mut Fixture,
    id: &ChannelId,
    format: MediaControlFormat,
    auto_answer: bool,
) -> Result<Peer, Error> {
    let mut options = WebSocketMediaOptions::new(MediaFormat::Pcmu);
    options.control_format = format;
    options.auto_answer = auto_answer;
    let channel = client
        .external_websocket_client(id, app, "agent", options, "media,agent")
        .await?;
    assert!(channel.name.starts_with("WebSocket/agent/"));
    let variables = channel
        .channelvars
        .ok_or(Error::Invalid("client media variables"))?;
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_CONNECTION_ID")
            .map(String::as_str),
        Some("agent")
    );
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE")
            .map(String::as_str),
        Some("160")
    );
    let mut peer = tokio::time::timeout(Duration::from_secs(2), fixture.sockets.recv())
        .await
        .map_err(invalid)?
        .ok_or(Error::Closed)?;
    let Message::Text(start) = tokio::time::timeout(Duration::from_secs(2), peer.next())
        .await
        .map_err(invalid)?
        .ok_or(Error::Closed)??
    else {
        return Err(Error::Invalid("client media start type"));
    };
    let event = MediaEvent::parse(&start, format, id).map_err(invalid)?;
    assert!(
        matches!(event, MediaEvent::MediaStart { connection_id, channel_id, format: MediaFormat::Pcmu, optimal_frame_size: 160, ptime: 20, .. } if connection_id.as_str() == "agent" && channel_id == *id)
    );
    if !auto_answer {
        assert_eq!(client.channel(id).await?.state, ChannelState::Down);
        peer.send(Message::Text(
            MediaCommand::Answer.encode(format).map_err(invalid)?.into(),
        ))
        .await?;
    }
    super::external_audio::started(session, id).await?;
    Ok(peer)
}

pub async fn compare(
    client: &Client,
    session: &mut Session,
    app: &str,
    fixture: &mut Fixture,
) -> Result<(), Error> {
    let ids = [
        ChannelId::try_from("client-left".to_owned())?,
        ChannelId::try_from("client-right".to_owned())?,
    ];
    let mut left = connect(
        client,
        session,
        app,
        fixture,
        &ids[0],
        MediaControlFormat::Json,
        true,
    )
    .await?;
    let mut right = connect(
        client,
        session,
        app,
        fixture,
        &ids[1],
        MediaControlFormat::Json,
        true,
    )
    .await?;
    let bridge = ChannelId::try_from("client-bridge".to_owned())?;
    client
        .create_bridge(&bridge, "Configured WebSocket comparison")
        .await?;
    client.add_channels(&bridge, &ids).await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut received = [false; 2];
        loop {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => { left.send(Message::Binary(vec![mulaw_encode(4000); 160].into())).await?; right.send(Message::Binary(vec![mulaw_encode(-7000); 160].into())).await?; sent += 1; }
                message = left.next() => received[0] |= audio(message, -7000)?,
                message = right.next() => received[1] |= audio(message, 4000)?,
            }
            if sent == 30 && received.iter().all(|received| *received) { return Ok::<(), Error>(()); }
        }
    }).await.map_err(invalid)??;
    client.remove_channels(&bridge, &ids).await?;
    controls(client, &mut left, &ids[0], MediaControlFormat::Json).await?;
    for id in &ids {
        client.hangup(id).await?;
    }
    client.destroy_bridge(&bridge).await?;
    let id = ChannelId::try_from("client-manual".to_owned())?;
    let mut peer = connect(
        client,
        session,
        app,
        fixture,
        &id,
        MediaControlFormat::PlainText,
        false,
    )
    .await?;
    controls(client, &mut peer, &id, MediaControlFormat::PlainText).await?;
    client.hangup(&id).await?;
    println!("Configured WebSocket received-audio and control comparison passed.");
    Ok(())
}
