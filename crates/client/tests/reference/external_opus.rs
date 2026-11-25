use super::external_websockets::{command, connect, expect};
use futures_util::{SinkExt, StreamExt};
use rusterisk_client::{
    api::{ChannelId, MediaCommand, MediaControlFormat, MediaDirection, MediaEvent, MediaFormat},
    Client, Error, Session,
};
use rusterisk_media::{Frame, OpusCodec};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn invalid(error: impl std::fmt::Display) -> Error {
    eprintln!("Opus WebSocket comparison: {error}");
    Error::Invalid("Opus WebSocket comparison")
}

fn signal(frequency: f64, frame: usize) -> Result<Frame, Error> {
    let mut samples = [0; 960];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = (6000.0
            * (std::f64::consts::TAU * frequency * (frame * 960 + index) as f64 / 48000.0).sin())
            as i16;
    }
    Frame::new(&samples, 48000).map_err(invalid)
}

fn audio(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    codec: &mut OpusCodec,
    wanted: f64,
    refused: f64,
) -> Result<bool, Error> {
    let bytes = match message.ok_or(Error::Closed)?? {
        Message::Binary(bytes) => bytes,
        Message::Text(_) | Message::Ping(_) | Message::Pong(_) => return Ok(false),
        Message::Close(_) | Message::Frame(_) => return Err(Error::Closed),
    };
    let frame = codec.decode(&bytes).map_err(invalid)?;
    let energy = |frequency: f64| {
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (index, sample) in frame.samples().iter().enumerate() {
            let phase = std::f64::consts::TAU * frequency * index as f64 / f64::from(frame.rate());
            real += f64::from(*sample) * phase.cos();
            imaginary += f64::from(*sample) * phase.sin();
        }
        (real * real + imaginary * imaginary) / frame.samples().len().pow(2) as f64
    };
    Ok(energy(wanted) > 100000.0 && energy(wanted) > energy(refused) * 10.0)
}

pub async fn compare(
    client: &Client,
    address: &str,
    session: &mut Session,
    app: &str,
) -> Result<(), Error> {
    let ids = [
        ChannelId::try_from("opus-left".to_owned())?,
        ChannelId::try_from("opus-right".to_owned())?,
    ];
    let format = MediaControlFormat::Json;
    let mut left = connect(
        client,
        address,
        session,
        app,
        &ids[0],
        format,
        MediaFormat::Opus,
    )
    .await?;
    let mut right = connect(
        client,
        address,
        session,
        app,
        &ids[1],
        format,
        MediaFormat::Opus,
    )
    .await?;
    for (name, operation) in [
        ("START_MEDIA_BUFFERING", MediaCommand::StartMediaBuffering),
        (
            "STOP_MEDIA_BUFFERING",
            MediaCommand::StopMediaBuffering {
                correlation_id: None,
            },
        ),
        (
            "MARK_MEDIA",
            MediaCommand::MarkMedia {
                correlation_id: None,
            },
        ),
        ("FLUSH_MEDIA", MediaCommand::FlushMedia),
        ("REPORT_QUEUE_DRAINED", MediaCommand::ReportQueueDrained),
        ("PAUSE_MEDIA", MediaCommand::PauseMedia),
        ("CONTINUE_MEDIA", MediaCommand::ContinueMedia),
        (
            "SET_MEDIA_DIRECTION",
            MediaCommand::SetMediaDirection {
                direction: MediaDirection::In,
            },
        ),
    ] {
        command(&mut left, operation, format).await?;
        expect(
            &mut left,
            MediaEvent::Error {
                channel_id: ids[0].clone(),
                error_text: format!("{name} not supported in passthrough mode"),
            },
            format,
        )
        .await?;
    }
    let bridge = rusterisk_client::BridgeId::try_from("opus-bridge".to_owned())?;
    client.create_bridge(&bridge, "Opus comparison").await?;
    client.add_channels(&bridge, &ids).await?;
    let mut left_audio = OpusCodec::new().map_err(invalid)?;
    let mut right_audio = OpusCodec::new().map_err(invalid)?;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut received = [false; 2];
        while sent < 30 || !received.into_iter().all(|value| value) {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    left.send(Message::Binary(left_audio.encode(signal(440.0, sent)?).map_err(invalid)?.to_vec().into())).await?;
                    right.send(Message::Binary(right_audio.encode(signal(660.0, sent)?).map_err(invalid)?.to_vec().into())).await?;
                    sent += 1;
                }
                message = left.next() => received[0] |= audio(message, &mut left_audio, 660.0, 440.0)?,
                message = right.next() => received[1] |= audio(message, &mut right_audio, 440.0, 660.0)?,
            }
        }
        Ok::<(), Error>(())
    }).await.map_err(invalid)??;
    for id in &ids {
        client.hangup(id).await?;
    }
    client.destroy_bridge(&bridge).await?;
    println!("Opus WebSocket control and received-audio comparison passed for {address}.");
    Ok(())
}
