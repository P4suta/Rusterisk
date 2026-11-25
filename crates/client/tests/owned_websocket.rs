use rusterisk_client::{
    api::{ChannelId, MediaControlFormat, MediaFormat, WebSocketMediaOptions},
    CallMilestone, Client, Error, MediaBytes, MediaMessage,
};
use rusterisk_media::{mulaw_decode, mulaw_encode};
use rusterisk_server::{
    http::router_with_media, media::MediaHub, signaling::Signaling, Controller,
};
use std::{future::IntoFuture, time::Duration};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::WebSocket(error.to_string())
}

#[tokio::test]
async fn owned_media_calls_receive_audio_and_release_channels_when_their_handles_close(
) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media.clone(), notices)
        .await
        .map_err(failure)?;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let http = tokio::spawn(
        axum::serve(listener, router_with_media(controller.clone(), media)).into_future(),
    );
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let session = client
            .subscribe(&ChannelId::try_from("calls".to_owned())?)
            .await?;
        let operation = async {
            let options = WebSocketMediaOptions::new(MediaFormat::Pcmu);
            let mut left = session.external_websocket(options, "left").await?;
            let mut right = session.external_websocket(options, "right").await?;
            let left_channel = left
                .wait(CallMilestone::Accepted, Duration::from_secs(1))
                .await?
                .channel
                .ok_or(Error::Closed)?;
            let right_channel = right
                .wait(CallMilestone::Accepted, Duration::from_secs(1))
                .await?
                .channel
                .ok_or(Error::Closed)?;
            let mut left_media = client
                .connect_media(&left_channel, MediaControlFormat::PlainText)
                .await?;
            let mut right_media = client
                .connect_media(&right_channel, MediaControlFormat::PlainText)
                .await?;
            for call in [&mut left, &mut right] {
                call.wait(CallMilestone::Stasis, Duration::from_secs(1))
                    .await?;
                call.wait(CallMilestone::Answered, Duration::from_secs(1))
                    .await?;
            }
            let bridge = rusterisk_client::BridgeId::try_from("audio".to_owned())?;
            client.create_bridge(&bridge, "SDK audio").await?;
            client
                .add_channels(&bridge, &[left.id().clone(), right.id().clone()])
                .await?;
            left_media
                .send_audio(MediaBytes::from(vec![mulaw_encode(4000); 160]))
                .await?;
            right_media
                .send_audio(MediaBytes::from(vec![mulaw_encode(-7000); 160]))
                .await?;
            let receive = async |media: &mut rusterisk_client::MediaSession, expected: i32| loop {
                match media.next_message().await? {
                    Some(MediaMessage::Audio(bytes))
                        if bytes.len() == 160
                            && bytes.iter().all(|byte| {
                                (i32::from(mulaw_decode(*byte)) - expected).abs() < 300
                            }) =>
                    {
                        return Ok::<(), Error>(())
                    }
                    Some(_) => {}
                    None => return Err(Error::Closed),
                }
            };
            tokio::try_join!(
                receive(&mut left_media, -7000),
                receive(&mut right_media, 4000)
            )?;
            for call in [&mut left, &mut right] {
                call.wait(CallMilestone::MediaConnected, Duration::from_secs(1))
                    .await?;
            }
            left.close().await?;
            right.close().await?;
            assert!(client.channels().await?.is_empty());
            for media in [&mut left_media, &mut right_media] {
                while media.next_message().await?.is_some() {}
            }
            client.destroy_bridge(&bridge).await?;
            Ok::<(), Error>(())
        }
        .await;
        let shutdown = session.shutdown().await;
        operation.and(shutdown)
    })
    .await
    .map_err(|_| Error::Timeout);
    controller.shutdown();
    let signaling_result = signaling.await.map_err(failure)?.map_err(failure);
    let media_result = hub.await.map_err(failure)?.map_err(failure);
    http.abort();
    assert!(http.await.is_err());
    result?.and(signaling_result).and(media_result)
}
