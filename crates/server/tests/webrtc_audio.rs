use rusterisk_config::Codec;
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::{
    media_link::Channels,
    rtp::SocketPair,
    webrtc::{WebRtcOffer, WebRtcSession, WebRtcStats},
};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
#[path = "webrtc_audio/media.rs"]
mod media;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

struct Application {
    incoming: mpsc::Receiver<Frame>,
    outgoing: mpsc::Sender<Frame>,
    stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
}

fn channels() -> (Channels, Application) {
    let (incoming, input) = mpsc::channel(3);
    let (digits, _digits) = mpsc::channel(16);
    let (outgoing, output) = mpsc::channel(3);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (directed, direction) = watch::channel(PeerMediaDirection::SendReceive);
    (
        Channels {
            incoming,
            digits,
            outgoing: output,
            commands,
            completed,
            stop: stopped,
            direction,
        },
        Application {
            incoming: input,
            outgoing,
            stop,
            _direction: directed,
        },
    )
}

fn signal(frequency: f64, frame: usize) -> Result<Frame, MediaError> {
    let mut samples = [0; 960];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = (6000.0
            * (std::f64::consts::TAU * frequency * (frame * 960 + index) as f64 / 48000.0).sin())
            as i16;
    }
    Frame::new(&samples, 48000)
}

fn energy(frame: &Frame, frequency: f64) -> f64 {
    let (mut real, mut imaginary) = (0.0, 0.0);
    for (index, sample) in frame.samples().iter().enumerate() {
        let phase = std::f64::consts::TAU * frequency * index as f64 / f64::from(frame.rate());
        real += f64::from(*sample) * phase.cos();
        imaginary += f64::from(*sample) * phase.sin();
    }
    (real * real + imaginary * imaginary) / frame.samples().len().pow(2) as f64
}

fn received(frame: &Frame, expected: f64, refused: f64) -> bool {
    let wanted = energy(frame, expected);
    wanted > 100000.0 && wanted > energy(frame, refused) * 10.0
}

async fn finished(
    mut task: tokio::task::JoinHandle<Result<WebRtcStats, MediaError>>,
) -> Result<WebRtcStats, MediaError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; task failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

async fn duplex(codec: Codec) -> Result<(), MediaError> {
    let left = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let right = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let left_address = left.local_addr().map_err(failure)?;
    let right_address = right.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(left, left_address, &[codec], Instant::now())?;
    let (right, answer) = WebRtcSession::answer(
        right,
        right_address,
        offer.description(),
        &[codec],
        Instant::now(),
    )?;
    let left = offer.accept_answer(&answer)?;
    assert!(!left.is_connected());
    assert!(!right.is_connected());
    let (left_channels, mut left_app) = channels();
    let (right_channels, mut right_app) = channels();
    let left = tokio::spawn(left.run(left_channels));
    let right = tokio::spawn(right.run(right_channels));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut received = [false; 2];
        while sent < 30 || !received.iter().all(|received| *received) {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    left_app.outgoing.try_send(signal(440.0, sent)?).map_err(failure)?;
                    right_app.outgoing.try_send(signal(660.0, sent)?).map_err(failure)?;
                    sent += 1;
                }
                frame = left_app.incoming.recv() => received[0] |= self::received(&frame.ok_or(MediaError::UnknownStream)?, 660.0, 440.0),
                frame = right_app.incoming.recv() => received[1] |= self::received(&frame.ok_or(MediaError::UnknownStream)?, 440.0, 660.0),
            }
        }
        Ok::<(), MediaError>(())
    }).await.map_err(failure);
    left_app.stop.send_replace(true);
    right_app.stop.send_replace(true);
    let left = finished(left).await;
    let right = finished(right).await;
    result??;
    for stats in [left?, right?] {
        assert!(stats.transport_connected);
        assert!(stats.audio.packets_received > 0);
        assert!(stats.audio.packets_sent > 0);
    }
    Ok(())
}

#[tokio::test]
async fn pcmu_media_delivers_both_known_audio_signals_and_releases_every_task(
) -> Result<(), MediaError> {
    duplex(Codec::Pcmu).await
}

#[tokio::test]
async fn pcma_media_delivers_both_known_audio_signals_and_releases_every_task(
) -> Result<(), MediaError> {
    duplex(Codec::Pcma).await
}

#[tokio::test]
async fn opus_media_delivers_both_known_audio_signals_and_releases_every_task(
) -> Result<(), MediaError> {
    duplex(Codec::Opus).await
}

#[tokio::test]
async fn a_mismatched_fingerprint_prevents_authenticated_audio() -> Result<(), MediaError> {
    let left = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let right = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let left_address = left.local_addr().map_err(failure)?;
    let right_address = right.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(left, left_address, &[Codec::Pcmu], Instant::now())?;
    let fingerprint = offer
        .description()
        .lines()
        .find_map(|line| line.strip_prefix("a=fingerprint:sha-256 "))
        .ok_or(MediaError::InvalidPacket)?;
    let changed = format!(
        "{}{}",
        if fingerprint.starts_with("00") {
            "01"
        } else {
            "00"
        },
        fingerprint.get(2..).ok_or(MediaError::InvalidPacket)?
    );
    let refused = offer.description().replace(fingerprint, &changed);
    let (right, answer) = WebRtcSession::answer(
        right,
        right_address,
        &refused,
        &[Codec::Pcmu],
        Instant::now(),
    )?;
    let left = offer.accept_answer(&answer)?;
    let (left_channels, mut left_app) = channels();
    let (right_channels, mut right_app) = channels();
    let left = tokio::spawn(left.run(left_channels));
    let right = tokio::spawn(right.run(right_channels));
    left_app
        .outgoing
        .try_send(signal(440.0, 0)?)
        .map_err(failure)?;
    let failed = finished(right).await;
    left_app.stop.send_replace(true);
    let left = finished(left).await?;
    assert!(
        matches!(failed, Err(MediaError::Transport(message)) if message.starts_with("WebRTC negotiation failed"))
    );
    assert!(left_app.incoming.try_recv().is_err());
    assert!(right_app.incoming.try_recv().is_err());
    assert_eq!(left.audio.packets_received, 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_the_ice_exchange_releases_the_reserved_media_ports() -> Result<(), MediaError> {
    let left = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let right = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let left_address = left.local_addr().map_err(failure)?;
    let right_address = right.local_addr().map_err(failure)?;
    let offer = WebRtcOffer::new(left, left_address, &[Codec::Pcmu], Instant::now())?;
    let (_right, answer) = WebRtcSession::answer(
        right,
        right_address,
        offer.description(),
        &[Codec::Pcmu],
        Instant::now(),
    )?;
    let left = offer.accept_answer(&answer)?;
    let (channels, mut application) = channels();
    let task = tokio::spawn(left.run(channels));
    let progress = tokio::time::timeout(Duration::from_secs(1), async {
        for frame in 0..4 {
            application
                .outgoing
                .send(signal(440.0, frame)?)
                .await
                .map_err(failure)?;
        }
        Ok::<(), MediaError>(())
    })
    .await
    .map_err(failure);
    application.stop.send_replace(true);
    let stats = finished(task).await?;
    progress??;
    assert!(!stats.transport_connected);
    assert_eq!(stats.audio.packets_received, 0);
    assert_eq!(stats.audio.packets_sent, 0);
    assert!(application.incoming.try_recv().is_err());
    let replacement = SocketPair::bind(
        left_address.ip(),
        left_address.port(),
        left_address.port() + 1,
    )
    .map_err(failure)?;
    assert_eq!(replacement.local_addr().map_err(failure)?, left_address);
    Ok(())
}
