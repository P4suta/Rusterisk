use super::{FrameClock, MediaHub, MediaNotice};
use crate::CallKey;
use rusterisk_api::ChannelId;
use rusterisk_media::MediaError;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

fn transport(error: std::io::Error) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key(generation: u64) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(format!("call-{generation}"))
            .map_err(|error| MediaError::Transport(error.to_string()))?,
        generation,
    })
}

#[tokio::test]
async fn all_release_notices_survive_a_full_notice_channel() -> Result<(), MediaError> {
    let (_port, mut hub, mut notices) = MediaHub::new(64);
    let first = key(1)?;
    for _ in 0..128 {
        hub.notice(MediaNotice::Answer { key: first.clone() })?;
    }
    for generation in 1..=64 {
        hub.release_call(key(generation)?)?;
    }
    assert_eq!(hub.release_notices.len(), 64);
    while let Ok(notice) = notices.try_recv() {
        assert!(matches!(notice, MediaNotice::Answer { .. }));
    }
    let (_shutdown, mut stop) = watch::channel(false);
    let mut clock = FrameClock::new();
    for generation in 1..=64 {
        hub.turn(&mut stop, &mut clock).await?;
        assert!(
            matches!(notices.try_recv(), Ok(MediaNotice::Released { key }) if key.generation == generation)
        );
    }
    assert!(hub.release_notices.is_empty());
    hub.close().await
}

#[tokio::test]
async fn a_stream_acknowledges_release_after_its_worker_exits() -> Result<(), MediaError> {
    let (_port, mut hub, mut notices) = MediaHub::new(2);
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(transport)?;
    let (client, accepted) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().map_err(transport)?),
        listener.accept()
    )
    .map_err(transport)?;
    let call = key(1)?;
    hub.attach_audiosocket(call.clone(), accepted.0, 8000)?;
    hub.release_call(call.clone())?;
    assert!(matches!(
        hub.check_link(&call),
        Err(MediaError::InvalidRoute)
    ));
    assert!(matches!(
        notices.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(hub.workers.len(), 1);
    let (_shutdown, mut stop) = watch::channel(false);
    hub.turn(&mut stop, &mut FrameClock::new()).await?;
    assert!(hub.workers.is_empty());
    assert!(hub.check_link(&call).is_ok());
    assert!(matches!(notices.try_recv(), Ok(MediaNotice::Released { key }) if key == call));
    drop(client);
    hub.close().await
}

#[tokio::test]
#[ignore = "measures simultaneous disconnects under a two-CPU limit"]
async fn sixty_four_active_rtp_disconnects_finish_within_one_frame() -> std::io::Result<()> {
    use crate::{Control, Effect, IncomingCall, IncomingTarget};
    use std::{
        collections::BTreeSet,
        io,
        net::Ipv4Addr,
        time::{Duration, Instant},
    };

    let (media, mut hub, mut notices) = MediaHub::new(64);
    let mut control = Control::new(64);
    let mut pending = BTreeSet::new();
    let ip = Ipv4Addr::LOCALHOST.into();
    let peer = super::SocketPair::bind(ip, 0, 0)?;
    for generation in 1..=64 {
        let id = key(generation).map_err(io::Error::other)?.id;
        control
            .incoming(
                IncomingCall {
                    id: id.clone(),
                    endpoint: "handset".into(),
                    caller: "100".into(),
                    context: "internal".into(),
                    extension: "100".into(),
                    target: IncomingTarget::Dialplan,
                },
                "incoming",
            )
            .map_err(io::Error::other)?;
        let call = control.key(&id).map_err(io::Error::other)?;
        hub.attach(
            call.clone(),
            super::SocketPair::bind(ip, 0, 0)?,
            super::RtpSession {
                remote: peer.local_addr()?,
                format: rusterisk_media::AudioFormat::Pcmu,
                payload_type: 0,
                telephone: None,
            },
            rusterisk_core::PeerMediaDirection::SendReceive,
            super::AttachmentMode::Answered,
        )
        .map_err(io::Error::other)?;
        pending.insert(call);
    }
    let (_shutdown, mut stop) = watch::channel(false);
    let mut frames = FrameClock::new();
    let start = Instant::now();
    let outcome = control.shutdown("disconnect").map_err(io::Error::other)?;
    for effect in outcome.effects {
        if let Effect::Release { key } = effect {
            media.release(key).map_err(io::Error::other)?;
        }
    }
    while !pending.is_empty() {
        hub.turn(&mut stop, &mut frames)
            .await
            .map_err(io::Error::other)?;
        while let Ok(notice) = notices.try_recv() {
            if let MediaNotice::Released { key } = notice {
                assert!(pending.remove(&key));
                assert!(control.media_released(&key));
            }
        }
    }
    let elapsed = start.elapsed();
    assert!(hub.legs.is_empty());
    hub.close().await.map_err(io::Error::other)?;
    println!(
        "simultaneous_rtp_disconnects=64 elapsed_us={}",
        elapsed.as_micros()
    );
    assert!(elapsed <= Duration::from_millis(20), "{elapsed:?}");
    Ok(())
}
