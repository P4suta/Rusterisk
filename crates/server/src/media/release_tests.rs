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
