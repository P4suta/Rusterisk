use rusterisk_api::{AudioSocketTarget, Channel, ChannelId, EventKind};
use rusterisk_config::Config;
use rusterisk_media::{AudioSocket, Frame};
use rusterisk_server::{
    media::MediaHub, signaling::Signaling, Action, ControlError, Controller, Credentials,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

#[tokio::test]
async fn external_streams_send_session_identity_receive_audio_and_release_the_connection(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let media_stop = controller.shutdown_signal();
    let signal_stop = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let mut subscription = controller.subscribe(credentials()?, None, "calls".into()).await?;
        let id = ChannelId::try_from("external".to_owned())?;
        let target = AudioSocketTarget::new(listener.local_addr().map_err(failure)?, [7; 16]).map_err(failure)?;
        let (response, (mut peer, _)) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::try_join!(controller.execute(credentials()?, None, Action::ExternalAudioSocket { id: id.clone(), app: "calls".into(), target }), async { listener.accept().await.map_err(failure) })
        }).await.map_err(failure)??;
        let channel: Channel = serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
        assert_eq!(channel.id, id);
        assert!(channel.channelvars.is_none());
        let mut identity = [0; 19];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut identity)).await.map_err(failure)?.map_err(failure)?;
        assert_eq!(AudioSocket::parse(&identity).map_err(failure)?, AudioSocket::Uuid([7; 16]));
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..32 {
                let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
                if matches!(event.kind, EventKind::StasisStart { channel, .. } if channel.id == id) { return Ok::<(), ControlError>(()); }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        peer.write_all(&AudioSocket::Audio(Frame::new(&[4000; 160], 8000).map_err(failure)?).encode().map_err(failure)?).await.map_err(failure)?;
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..32 {
                let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
                if matches!(event.kind, EventKind::ChannelVarset { variable, value, .. } if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1") { return Ok::<(), ControlError>(()); }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        controller.execute(credentials()?, None, Action::Hangup(id)).await?;
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut bytes = [0; 1923];
            for _ in 0..100 {
                peer.read_exact(&mut bytes[..3]).await.map_err(failure)?;
                let length = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
                if length > 1920 { return Err(ControlError::Capacity); }
                peer.read_exact(&mut bytes[3..length + 3]).await.map_err(failure)?;
                if matches!(AudioSocket::parse(&bytes[..length + 3]).map_err(failure)?, AudioSocket::Terminate) {
                    assert_eq!(peer.read(&mut bytes[..1]).await.map_err(failure)?, 0);
                    return Ok::<(), ControlError>(());
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
