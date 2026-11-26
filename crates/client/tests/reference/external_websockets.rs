use futures_util::{SinkExt, StreamExt};
use rusterisk_client::{
    api::{
        ChannelId, MediaCommand, MediaControlFormat, MediaEvent, MediaFormat, WebSocketMediaOptions,
    },
    Client, DtmfDigit, DtmfTiming, Error, Session,
};
use rusterisk_media::{mulaw_decode, mulaw_encode};
use rusterisk_server::test_time::Wait;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("Media WebSocket comparison: {error}");
    Error::Invalid("media WebSocket comparison")
}

pub(super) type Peer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) async fn connect(
    client: &Client,
    address: &str,
    session: &mut Session,
    app: &str,
    id: &ChannelId,
    format: MediaControlFormat,
    media_format: MediaFormat,
) -> Result<Peer, Error> {
    let mut options = WebSocketMediaOptions::new(media_format);
    options.control_format = format;
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
        Some(media_format.optimal_frame_size().to_string()).as_deref()
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
    let (mut peer, response) = Wait::Ready
        .bounded(tokio_tungstenite::connect_async(request))
        .await
        .map_err(invalid)??;
    assert_eq!(response.headers()["Sec-WebSocket-Protocol"], "media");
    let start = Wait::Ready
        .bounded(peer.next())
        .await
        .map_err(invalid)?
        .ok_or(Error::Closed)??;
    let Message::Text(text) = start else {
        return Err(Error::Invalid("media start type"));
    };
    match format {
        MediaControlFormat::Json => {
            let start: MediaEvent = serde_json::from_str(&text)?;
            assert!(matches!(start, MediaEvent::MediaStart { connection_id, channel_id, format, optimal_frame_size, ptime: 20, .. } if connection_id == connection && channel_id == *id && format == media_format && optimal_frame_size == media_format.optimal_frame_size()));
        }
        MediaControlFormat::PlainText => assert_eq!(text.as_str(), format!("MEDIA_START connection_id:{connection} channel:{} channel_id:{id} format:{} optimal_frame_size:{} ptime:20", channel.name, media_format.as_str(), media_format.optimal_frame_size())),
    }
    super::external_audio::started(session, id).await?;
    status(&mut peer, id, format, 0, false, false).await?;
    Ok(peer)
}

pub(super) async fn command(
    peer: &mut Peer,
    command: MediaCommand,
    format: MediaControlFormat,
) -> Result<(), Error> {
    peer.send(Message::Text(
        command.encode(format).map_err(invalid)?.into(),
    ))
    .await?;
    Ok(())
}

pub(super) async fn expect(
    peer: &mut Peer,
    event: MediaEvent,
    format: MediaControlFormat,
) -> Result<(), Error> {
    Wait::Ready
        .bounded(async {
            loop {
                match peer.next().await.ok_or(Error::Closed)?? {
                    Message::Text(text) => {
                        match format {
                            MediaControlFormat::Json => {
                                assert_eq!(serde_json::from_str::<MediaEvent>(&text)?, event)
                            }
                            MediaControlFormat::PlainText => {
                                assert_eq!(text.as_str(), event.encode(format)?.as_str())
                            }
                        }
                        return Ok(());
                    }
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) | Message::Frame(_) => return Err(Error::Closed),
                }
            }
        })
        .await
        .map_err(|error| invalid(format!("waiting for {event:?}: {error}")))?
}

async fn status(
    peer: &mut Peer,
    id: &ChannelId,
    format: MediaControlFormat,
    frames: u16,
    bulk: bool,
    paused: bool,
) -> Result<(), Error> {
    command(peer, MediaCommand::GetStatus, format).await?;
    expect(
        peer,
        MediaEvent::Status {
            channel_id: id.clone(),
            queue_length: frames,
            xon_level: 800,
            xoff_level: 900,
            queue_full: false,
            bulk_media: bulk,
            media_paused: paused,
        },
        format,
    )
    .await
}

pub(super) async fn controls(
    client: &Client,
    peer: &mut Peer,
    id: &ChannelId,
    format: MediaControlFormat,
) -> Result<(), Error> {
    command(peer, MediaCommand::FlushMedia, format).await?;
    status(peer, id, format, 0, false, false).await?;
    match format {
        MediaControlFormat::PlainText => {
            peer.send(Message::Text("SET_MEDIA_DIRECTION both".into()))
                .await?;
            expect(
                peer,
                MediaEvent::Error {
                    channel_id: id.clone(),
                    error_text: "SET_MEDIA_DIRECTION only supports JSON format.\n".into(),
                },
                format,
            )
            .await?;
        }
        MediaControlFormat::Json => {
            command(
                peer,
                MediaCommand::SetMediaDirection {
                    direction: rusterisk_client::api::MediaDirection::In,
                },
                format,
            )
            .await?;
            command(peer, MediaCommand::StartMediaBuffering, format).await?;
            expect(
                peer,
                MediaEvent::Error {
                    channel_id: id.clone(),
                    error_text: "START_MEDIA_BUFFERING not supported while media direction is 'in'"
                        .into(),
                },
                format,
            )
            .await?;
            command(
                peer,
                MediaCommand::SetMediaDirection {
                    direction: rusterisk_client::api::MediaDirection::Both,
                },
                format,
            )
            .await?;
        }
    }
    command(peer, MediaCommand::PauseMedia, format).await?;
    command(peer, MediaCommand::StartMediaBuffering, format).await?;
    peer.send(Message::Binary(vec![mulaw_encode(4000); 80].into()))
        .await?;
    peer.send(Message::Binary(vec![mulaw_encode(4000); 80].into()))
        .await?;
    command(
        peer,
        MediaCommand::MarkMedia {
            correlation_id: Some("first".into()),
        },
        format,
    )
    .await?;
    peer.send(Message::Binary(vec![mulaw_encode(-7000); 160].into()))
        .await?;
    status(peer, id, format, 2, true, true).await?;
    command(
        peer,
        MediaCommand::StopMediaBuffering {
            correlation_id: Some("finished".into()),
        },
        format,
    )
    .await?;
    command(peer, MediaCommand::ReportQueueDrained, format).await?;
    status(peer, id, format, 2, false, true).await?;
    command(peer, MediaCommand::ContinueMedia, format).await?;
    expect(
        peer,
        MediaEvent::MediaMarkProcessed {
            channel_id: id.clone(),
            correlation_id: "first".into(),
        },
        format,
    )
    .await?;
    expect(
        peer,
        MediaEvent::MediaBufferingCompleted {
            channel_id: id.clone(),
            correlation_id: "finished".into(),
        },
        format,
    )
    .await?;
    expect(
        peer,
        MediaEvent::QueueDrained {
            channel_id: id.clone(),
        },
        format,
    )
    .await?;
    status(peer, id, format, 0, false, false).await?;
    client
        .send_dtmf(
            id,
            &[DtmfDigit::Hash, DtmfDigit::A],
            DtmfTiming::new(0, 80, 100, 0).map_err(|_| Error::Invalid("digit timing"))?,
        )
        .await?;
    for digit in ['#', 'A'] {
        expect(
            peer,
            MediaEvent::DtmfEnd {
                channel_id: id.clone(),
                digit,
            },
            format,
        )
        .await?;
    }
    command(peer, MediaCommand::PauseMedia, format).await?;
    command(peer, MediaCommand::StartMediaBuffering, format).await?;
    peer.send(Message::Binary(vec![mulaw_encode(9000); 160].into()))
        .await?;
    command(
        peer,
        MediaCommand::MarkMedia {
            correlation_id: Some("discarded".into()),
        },
        format,
    )
    .await?;
    command(peer, MediaCommand::ReportQueueDrained, format).await?;
    command(peer, MediaCommand::FlushMedia, format).await?;
    status(peer, id, format, 0, false, true).await?;
    command(peer, MediaCommand::ContinueMedia, format).await?;
    expect(
        peer,
        MediaEvent::QueueDrained {
            channel_id: id.clone(),
        },
        format,
    )
    .await?;
    status(peer, id, format, 0, false, false).await?;
    pressure(peer, id, format).await?;
    Ok(())
}

async fn pressure(
    peer: &mut Peer,
    id: &ChannelId,
    format: MediaControlFormat,
) -> Result<(), Error> {
    command(peer, MediaCommand::PauseMedia, format).await?;
    status(peer, id, format, 0, false, true).await?;
    for frames in [400, 400, 100] {
        peer.send(Message::Binary(vec![0xff; frames * 160].into()))
            .await?;
    }
    expect(
        peer,
        MediaEvent::MediaXoff {
            channel_id: id.clone(),
        },
        format,
    )
    .await?;
    command(peer, MediaCommand::GetStatus, format).await?;
    expect(
        peer,
        MediaEvent::Status {
            channel_id: id.clone(),
            queue_length: 900,
            xon_level: 800,
            xoff_level: 900,
            queue_full: true,
            bulk_media: false,
            media_paused: true,
        },
        format,
    )
    .await?;
    command(peer, MediaCommand::ReportQueueDrained, format).await?;
    command(peer, MediaCommand::FlushMedia, format).await?;
    command(peer, MediaCommand::GetStatus, format).await?;
    expect(
        peer,
        MediaEvent::Status {
            channel_id: id.clone(),
            queue_length: 0,
            xon_level: 800,
            xoff_level: 900,
            queue_full: true,
            bulk_media: false,
            media_paused: true,
        },
        format,
    )
    .await?;
    command(peer, MediaCommand::ContinueMedia, format).await?;
    expect(
        peer,
        MediaEvent::MediaXon {
            channel_id: id.clone(),
        },
        format,
    )
    .await?;
    expect(
        peer,
        MediaEvent::QueueDrained {
            channel_id: id.clone(),
        },
        format,
    )
    .await?;
    status(peer, id, format, 0, false, false).await
}

pub(super) fn audio(
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
    let mut left = connect(
        client,
        address,
        session,
        app,
        &ids[0],
        MediaControlFormat::Json,
        MediaFormat::Pcmu,
    )
    .await?;
    let mut right = connect(
        client,
        address,
        session,
        app,
        &ids[1],
        MediaControlFormat::Json,
        MediaFormat::Pcmu,
    )
    .await?;
    let bridge = rusterisk_client::BridgeId::try_from("websocket-bridge".to_owned())?;
    client
        .create_bridge(&bridge, "WebSocket comparison")
        .await?;
    client.add_channels(&bridge, &ids).await?;
    Wait::Ready
        .bounded(async {
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
    controls(client, &mut left, &ids[0], MediaControlFormat::Json).await?;
    for id in &ids {
        client.hangup(id).await?;
    }
    client.destroy_bridge(&bridge).await?;
    let id = ChannelId::try_from("websocket-plain".to_owned())?;
    let mut plain = connect(
        client,
        address,
        session,
        app,
        &id,
        MediaControlFormat::PlainText,
        MediaFormat::Pcmu,
    )
    .await?;
    controls(client, &mut plain, &id, MediaControlFormat::PlainText).await?;
    client.hangup(&id).await?;
    println!("WebSocket control and received-audio comparison passed for {address}.");
    Ok(())
}
