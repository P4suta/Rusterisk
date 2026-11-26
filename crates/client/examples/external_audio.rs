use rusterisk_client::{
    api::{ChannelId, MediaControlFormat, MediaFormat, WebSocketMediaOptions},
    CallMilestone, Client, Error, MediaBytes, MediaMessage, OwnedCall,
};
use std::{future::Future, time::Duration};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Error> {
    let client = Client::new(
        &setting("RUSTERISK_URL", Some("http://127.0.0.1:8088"))?,
        &setting("RUSTERISK_USERNAME", Some("api"))?,
        &setting("RUSTERISK_PASSWORD", None)?,
    )?;
    run_audio(
        client,
        ChannelId::try_from(setting("RUSTERISK_APP", Some("calls"))?)?,
        rusterisk_client::BridgeId::try_from(setting("RUSTERISK_BRIDGE", None)?)?,
        async { tokio::signal::ctrl_c().await.map_err(Error::from) },
    )
    .await
}

fn setting(name: &'static str, default: Option<&str>) -> Result<String, Error> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => default
            .map(str::to_owned)
            .ok_or(Error::Invalid("required example setting is missing")),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::Invalid("example settings require Unicode"))
        }
    }
}

async fn run_audio(
    client: Client,
    application: ChannelId,
    bridge: rusterisk_client::BridgeId,
    stop: impl Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    let session = client.subscribe(&application).await?;
    let result = async {
        let options = WebSocketMediaOptions::new(MediaFormat::Pcm16);
        let mut call = session
            .external_websocket(options, "external_audio")
            .await?;
        let result = tokio::select! {
            stopped = stop => stopped,
            exchanged = exchange(&client, &mut call, &bridge) => exchanged,
        };
        let cleanup = call.close().await;
        if let Err(error) = &cleanup {
            eprintln!("{error}");
        }
        result.and(cleanup)
    }
    .await;
    let shutdown = session.shutdown().await;
    if let Err(error) = &shutdown {
        eprintln!("{error}");
    }
    result.and(shutdown)
}

fn tone_frame(phase: u32) -> [u8; 640] {
    let mut bytes = [0; 640];
    for (index, sample) in bytes.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let angle = std::f32::consts::TAU * 440.0 * (phase as f32 + index as f32) / 16000.0;
        sample.copy_from_slice(&((angle.sin() * 1200.0) as i16).to_le_bytes());
    }
    bytes
}

async fn exchange(
    client: &Client,
    call: &mut OwnedCall,
    bridge: &rusterisk_client::BridgeId,
) -> Result<(), Error> {
    let accepted = call
        .wait(CallMilestone::Accepted, Duration::from_secs(30))
        .await?;
    let channel = accepted.channel.ok_or(Error::Closed)?;
    let mut media = client
        .connect_media(&channel, MediaControlFormat::PlainText)
        .await?;
    call.wait(CallMilestone::Stasis, Duration::from_secs(30))
        .await?;
    call.wait(CallMilestone::Answered, Duration::from_secs(30))
        .await?;
    client
        .add_channels(bridge, std::slice::from_ref(call.id()))
        .await?;
    println!("{} exchanging audio through {bridge}", call.id());
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(deadline);
    let mut phase = 0;
    let mut received = 0_u64;
    loop {
        tokio::select! {
            () = &mut deadline => return Ok(()),
            _instant = clock.tick() => {
                media.send_audio(MediaBytes::copy_from_slice(&tone_frame(phase))).await?;
                phase = (phase + 320) % 16000;
            },
            message = media.next_message() => match message? {
                Some(MediaMessage::Audio(bytes)) => {
                    received = received.saturating_add(bytes.len() as u64);
                    if received.is_multiple_of(32000) { println!("{} received {received} audio bytes", call.id()); }
                },
                Some(MediaMessage::Event(_)) => {},
                None => return Err(Error::Closed),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rusterisk_client::{api::ChannelId, Client, Error};
    use rusterisk_server::{
        http::router_with_media, media::MediaHub, signaling::Signaling, Controller,
    };
    use std::future::IntoFuture;

    fn failure(error: impl std::fmt::Display) -> Error {
        Error::WebSocket(error.to_string())
    }

    #[tokio::test]
    async fn stopping_external_audio_releases_its_channel_and_preserves_the_existing_bridge(
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
        let result = rusterisk_server::test_time::Wait::Ready
            .bounded(async {
                let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
                let bridge = rusterisk_client::BridgeId::try_from("existing".to_owned())?;
                client.create_bridge(&bridge, "Existing audio").await?;
                let (stop, stopped) = tokio::sync::oneshot::channel();
                let observe = async {
                    loop {
                        for channel in client.channels().await? {
                            match client
                                .variable(&channel.id, "RUSTERISK_MEDIA_CONNECTED")
                                .await
                            {
                                Ok(variable) if variable.value == "1" => {
                                    stop.send(()).map_err(|_| Error::Closed)?;
                                    return Ok::<(), Error>(());
                                }
                                Ok(_) | Err(Error::Api { status: 409, .. }) => {}
                                Err(error) => return Err(error),
                            }
                        }
                    }
                };
                tokio::try_join!(
                    super::run_audio(
                        client.clone(),
                        ChannelId::try_from("calls".to_owned())?,
                        bridge.clone(),
                        async { stopped.await.map_err(|_| Error::Closed) }
                    ),
                    observe
                )?;
                assert!(client.channels().await?.is_empty());
                assert!(client.bridge(&bridge).await?.channels.is_empty());
                client.destroy_bridge(&bridge).await?;
                Ok::<(), Error>(())
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
}
