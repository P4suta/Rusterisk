use futures_util::{SinkExt, StreamExt};
use rusterisk_client::{
    api::{ChannelId, MediaControlFormat, MediaEvent, MediaFormat, WebSocketMediaOptions},
    Client, Error, Session,
};
use rusterisk_media::{mulaw_decode, mulaw_encode};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("Media WebSocket comparison: {error}");
    Error::Invalid("media WebSocket comparison")
}

type Peer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(
    client: &Client,
    address: &str,
    session: &mut Session,
    app: &str,
    id: &ChannelId,
) -> Result<Peer, Error> {
    let mut options = WebSocketMediaOptions::new(MediaFormat::Pcmu);
    options.control_format = MediaControlFormat::Json;
    let channel = client
        .external_websocket(id, app, options, "media,agent")
        .await?;
    assert_eq!(channel.id, *id);
    assert!(channel.name.starts_with("WebSocket/INCOMING/"));
    let mut variables = channel
        .channelvars
        .ok_or(Error::Invalid("media variables"))?;
    assert_eq!(
        variables
            .remove("MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE")
            .as_deref(),
        Some("160")
    );
    let connection = ChannelId::try_from(
        variables
            .remove("MEDIA_WEBSOCKET_CONNECTION_ID")
            .ok_or(Error::Invalid("connection identifier"))?,
    )?;
    let mut url = url::Url::parse(address).map_err(invalid)?;
    url.set_scheme(if address.starts_with("https:") {
        "wss"
    } else {
        "ws"
    })
    .map_err(|()| Error::Invalid("media scheme"))?;
    url.set_path(&format!("/media/{connection}"));
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "media".parse().map_err(invalid)?);
    let (mut peer, response) = tokio::time::timeout(
        Duration::from_secs(2),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(invalid)??;
    assert_eq!(response.headers()["Sec-WebSocket-Protocol"], "media");
    let start = tokio::time::timeout(Duration::from_secs(2), peer.next())
        .await
        .map_err(invalid)?
        .ok_or(Error::Closed)??;
    let Message::Text(text) = start else {
        return Err(Error::Invalid("media start type"));
    };
    let start: MediaEvent = serde_json::from_str(&text)?;
    assert!(
        matches!(start, MediaEvent::MediaStart { connection_id, channel_id, format: MediaFormat::Pcmu, optimal_frame_size: 160, ptime: 20, .. } if connection_id == connection && channel_id == *id)
    );
    super::external_audio::started(session, id).await?;
    peer.send(Message::Text(r#"{"command":"GET_STATUS"}"#.into()))
        .await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let message = peer.next().await.ok_or(Error::Closed)??;
            if let Message::Text(text) = message {
                assert!(matches!(serde_json::from_str::<MediaEvent>(&text)?, MediaEvent::Status { channel_id, xon_level: 800, xoff_level: 900, bulk_media: false, media_paused: false, .. } if channel_id == *id));
                return Ok::<(), Error>(());
            }
        }
    }).await.map_err(invalid)??;
    Ok(peer)
}

fn audio(
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

pub async fn compare(
    client: &Client,
    address: &str,
    session: &mut Session,
    app: &str,
) -> Result<(), Error> {
    let ids = [
        ChannelId::try_from("websocket-left".to_owned())?,
        ChannelId::try_from("websocket-right".to_owned())?,
    ];
    let mut left = connect(client, address, session, app, &ids[0]).await?;
    let mut right = connect(client, address, session, app, &ids[1]).await?;
    let bridge = ChannelId::try_from("websocket-bridge".to_owned())?;
    client
        .create_bridge(&bridge, "WebSocket comparison")
        .await?;
    client.add_channels(&bridge, &ids).await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut left_received = false;
        let mut right_received = false;
        loop {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    left.send(Message::Binary(vec![mulaw_encode(4000); 160].into())).await?;
                    right.send(Message::Binary(vec![mulaw_encode(-7000); 160].into())).await?;
                    sent += 1;
                }
                message = left.next() => left_received |= audio(message, -7000)?,
                message = right.next() => right_received |= audio(message, 4000)?,
            }
            if sent == 30 && left_received && right_received {
                return Ok::<(), Error>(());
            }
        }
    })
    .await
    .map_err(invalid)??;
    client.remove_channels(&bridge, &ids).await?;
    for id in &ids {
        client.hangup(id).await?;
    }
    client.destroy_bridge(&bridge).await?;
    Ok(())
}
