use axum::extract::ws::WebSocketUpgrade;
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_media::{mulaw_decode, mulaw_encode, Frame, MediaError, OpusCodec};
use rusterisk_server::{
    media::{MediaHub, MediaNotice},
    media_websocket::{MediaLinkOptions, MediaSocket, SocketMessage},
    CallKey,
};
use std::{future::IntoFuture, time::Duration};
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key(id: &str, generation: u64) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(id.to_owned()).map_err(failure)?,
        generation,
    })
}

fn options(key: &CallKey, format: MediaFormat) -> MediaLinkOptions {
    MediaLinkOptions {
        connection_id: key.id.clone(),
        channel_id: key.id.clone(),
        channel: format!("WebSocket/INCOMING/{}", key.id),
        format,
        control_format: MediaControlFormat::Json,
        direction: MediaDirection::Both,
        auto_answer: true,
        channel_variables: Default::default(),
    }
}

enum WireAudio {
    Pcmu,
    Opus(OpusCodec),
}

impl WireAudio {
    fn new(format: MediaFormat) -> Result<Self, MediaError> {
        match format {
            MediaFormat::Pcmu => Ok(Self::Pcmu),
            MediaFormat::Opus => Ok(Self::Opus(OpusCodec::new()?)),
            MediaFormat::Pcma
            | MediaFormat::Pcm8
            | MediaFormat::Pcm12
            | MediaFormat::Pcm16
            | MediaFormat::Pcm24
            | MediaFormat::Pcm32
            | MediaFormat::Pcm44
            | MediaFormat::Pcm48 => Err(MediaError::Unsupported),
        }
    }

    fn encoded(
        &mut self,
        amplitude: i16,
        frequency: f64,
        sequence: usize,
    ) -> Result<Vec<u8>, MediaError> {
        match self {
            Self::Pcmu => Ok(vec![mulaw_encode(amplitude); 160]),
            Self::Opus(codec) => {
                let mut samples = [0; 960];
                for (index, sample) in samples.iter_mut().enumerate() {
                    *sample = (6000.0
                        * (std::f64::consts::TAU * frequency * (sequence * 960 + index) as f64
                            / 48000.0)
                            .sin()) as i16;
                }
                codec.encode(Frame::new(&samples, 48000)?)
            }
        }
    }

    fn received(
        &mut self,
        bytes: &[u8],
        amplitude: i16,
        wanted: f64,
        refused: f64,
    ) -> Result<bool, MediaError> {
        match self {
            Self::Pcmu => Ok(bytes.len() == 160
                && bytes.iter().all(|byte| {
                    (i32::from(mulaw_decode(*byte)) - i32::from(amplitude)).abs() < 300
                })),
            Self::Opus(codec) => {
                let frame = codec.decode(bytes)?;
                let energy = |frequency: f64| {
                    let (mut real, mut imaginary) = (0.0, 0.0);
                    for (index, sample) in frame.samples().iter().enumerate() {
                        let phase = std::f64::consts::TAU * frequency * index as f64
                            / f64::from(frame.rate());
                        real += f64::from(*sample) * phase.cos();
                        imaginary += f64::from(*sample) * phase.sin();
                    }
                    (real * real + imaginary * imaginary) / frame.samples().len().pow(2) as f64
                };
                Ok(energy(wanted) > 100000.0 && energy(wanted) > energy(refused) * 10.0)
            }
        }
    }
}

async fn duplex(format: MediaFormat) -> Result<(), MediaError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (sender, mut sockets) = mpsc::channel(2);
    let app = axum::Router::new().route(
        "/media",
        axum::routing::get(move |upgrade: WebSocketUpgrade| {
            let sender = sender.clone();
            async move {
                upgrade.on_upgrade(async move |socket| {
                    assert!(sender.try_send(socket).is_ok());
                })
            }
        }),
    );
    let http = tokio::spawn(axum::serve(listener, app).into_future());
    let (media, hub, mut notices) = MediaHub::new(4);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let (mut left, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let left_key = key("left", 1)?;
        media
            .attach_websocket(
                left_key.clone(),
                sockets.recv().await.ok_or(MediaError::UnknownStream)?,
                options(&left_key, format),
            )
            .await?;
        let (mut right, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let right_key = key("right", 2)?;
        media
            .attach_websocket(
                right_key.clone(),
                sockets.recv().await.ok_or(MediaError::UnknownStream)?,
                options(&right_key, format),
            )
            .await?;
        assert!(matches!(
            MediaSocket::receive(&mut left).await?,
            Some(SocketMessage::Text(_))
        ));
        assert!(matches!(
            MediaSocket::receive(&mut right).await?,
            Some(SocketMessage::Text(_))
        ));
        let mut answers = Vec::with_capacity(2);
        while answers.len() < 2 {
            if let Some(MediaNotice::Answer { key }) = notices.recv().await {
                answers.push(key);
            }
        }
        assert!(answers.contains(&left_key) && answers.contains(&right_key));
        media.bridge(
            1,
            [left_key.clone(), right_key.clone()].into_iter().collect(),
        )?;
        let mut left_audio = WireAudio::new(format)?;
        let mut right_audio = WireAudio::new(format)?;
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut received = [false; 2];
        while sent < 30 || !received.into_iter().all(|value| value) {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    MediaSocket::send(&mut left, SocketMessage::Binary(left_audio.encoded(4000, 440.0, sent)?.into())).await?;
                    MediaSocket::send(&mut right, SocketMessage::Binary(right_audio.encoded(-7000, 660.0, sent)?.into())).await?;
                    sent += 1;
                }
                message = MediaSocket::receive(&mut left) => match message? {
                    Some(SocketMessage::Binary(bytes)) => received[0] |= left_audio.received(&bytes, -7000, 660.0, 440.0)?,
                    None | Some(SocketMessage::Close) => return Err(MediaError::UnknownStream),
                    Some(SocketMessage::Text(_) | SocketMessage::Ping(_) | SocketMessage::Pong(_)) => {},
                },
                message = MediaSocket::receive(&mut right) => match message? {
                    Some(SocketMessage::Binary(bytes)) => received[1] |= right_audio.received(&bytes, 4000, 440.0, 660.0)?,
                    None | Some(SocketMessage::Close) => return Err(MediaError::UnknownStream),
                    Some(SocketMessage::Text(_) | SocketMessage::Ping(_) | SocketMessage::Pong(_)) => {},
                },
            }
        }
        MediaSocket::send(&mut left, SocketMessage::Close).await?;
        loop {
            match MediaSocket::receive(&mut left).await? {
                Some(SocketMessage::Close) => break,
                Some(
                    SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_),
                ) => {}
                Some(SocketMessage::Ping(bytes)) => {
                    MediaSocket::send(&mut left, SocketMessage::Pong(bytes)).await?
                }
                None => return Err(MediaError::InvalidPacket),
            }
        }
        loop {
            if let Some(MediaNotice::Stopped { key, result }) = notices.recv().await {
                assert_eq!(key, left_key);
                result?;
                break;
            }
        }
        media.release(right_key)?;
        Ok::<(), MediaError>(())
    })
    .await
    .map_err(failure);
    stop.send_replace(true);
    task.await.map_err(failure)??;
    http.abort();
    assert!(http.await.is_err());
    match result? {
        Ok(()) => Ok(()),
        Err(primary) => {
            while let Ok(notice) = notices.try_recv() {
                if let MediaNotice::Stopped {
                    key,
                    result: Err(error),
                } = notice
                {
                    return Err(failure(format!(
                        "{primary}; media link {} failed: {error}",
                        key.id
                    )));
                }
            }
            Err(primary)
        }
    }
}

#[tokio::test]
async fn websocket_peers_exchange_known_audio_through_the_media_hub() -> Result<(), MediaError> {
    duplex(MediaFormat::Pcmu).await
}

#[tokio::test]
async fn websocket_peers_exchange_known_opus_packets_and_release_owned_tasks(
) -> Result<(), MediaError> {
    duplex(MediaFormat::Opus).await
}
