use axum::{
    extract::ws::WebSocketUpgrade,
    http::{HeaderMap, Uri},
};
use rusterisk_api::{
    Channel, ChannelId, EventKind, MediaControlFormat, MediaDirection, MediaEvent, MediaFormat,
};
use rusterisk_config::Config;
use rusterisk_media::{mulaw_decode, mulaw_encode};
use rusterisk_server::{
    media::MediaHub,
    media_websocket::{MediaSocket, SocketMessage},
    signaling::Signaling,
    Action, ControlError, Controller, Credentials,
};
use std::{future::IntoFuture, time::Duration};
use tokio::sync::mpsc;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn create(id: ChannelId) -> Action {
    Action::ExternalWebSocketClient {
        id,
        app: "calls".into(),
        client: "agent".into(),
        format: MediaFormat::Pcmu,
        control_format: Some(MediaControlFormat::Json),
        direction: MediaDirection::Both,
        auto_answer: true,
        data: "media,agent".into(),
    }
}

#[tokio::test]
async fn configured_client_connections_preserve_authentication_and_exchange_received_audio(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (sender, mut sockets) = mpsc::channel(2);
    let app = axum::Router::new().route(
        "/agent",
        axum::routing::get(
            move |uri: Uri, headers: HeaderMap, upgrade: WebSocketUpgrade| {
                let sender = sender.clone();
                async move {
                    assert_eq!(uri.query(), Some("token=private-query"));
                    assert_eq!(headers["Authorization"], "Basic cGVlcjp0ZXN0LXBhc3N3b3Jk");
                    upgrade
                        .protocols(["media"])
                        .on_upgrade(async move |socket| {
                            assert!(sender.try_send(socket).is_ok());
                        })
                }
            },
        ),
    );
    let http = tokio::spawn(axum::serve(listener, app).into_future());
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?.with_websocket_configuration("", &format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{address}/agent?token=private-query\nprotocols=media\nusername=peer\npassword=test-password\nreconnect_attempts=1\n")).map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut subscription = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let ids = [id("client-left")?, id("client-right")?];
        let mut peers = Vec::with_capacity(2);
        for id in &ids {
            let accepted = controller.execute(credentials.clone(), None, create(id.clone())).await?;
            let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
            assert_eq!(channel.id, *id);
            let mut peer = sockets.recv().await.ok_or(ControlError::Capacity)?;
            let Some(SocketMessage::Text(start)) = MediaSocket::receive(&mut peer).await.map_err(failure)? else { return Err(ControlError::Unsupported); };
            assert!(matches!(serde_json::from_str::<MediaEvent>(&start)?, MediaEvent::MediaStart { connection_id, channel_id, .. } if connection_id.as_str() == "agent" && channel_id == *id));
            loop { if let EventKind::StasisStart { channel, args } = subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind { if channel.id == *id { assert_eq!(args, ["media", "agent"]); break; } } }
            peers.push(peer);
        }
        let bridge = id("bridge")?;
        controller.execute(credentials.clone(), None, Action::CreateBridge { id: bridge.clone(), name: "Client audio".into() }).await?;
        controller.execute(credentials.clone(), None, Action::AddChannels { bridge: bridge.clone(), channels: ids.clone().into_iter().collect() }).await?;
        let mut left = peers.remove(0);
        let mut right = peers.remove(0);
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut received = [false; 2];
        while sent < 30 || !received.iter().all(|value| *value) {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => { MediaSocket::send(&mut left, SocketMessage::Binary(vec![mulaw_encode(4000); 160].into())).await.map_err(failure)?; MediaSocket::send(&mut right, SocketMessage::Binary(vec![mulaw_encode(-7000); 160].into())).await.map_err(failure)?; sent += 1; }
                message = MediaSocket::receive(&mut left) => { if let Some(SocketMessage::Binary(bytes)) = message.map_err(failure)? { received[0] |= bytes.len() == 160 && bytes.iter().all(|sample| (i32::from(mulaw_decode(*sample)) + 7000).abs() < 300); } }
                message = MediaSocket::receive(&mut right) => { if let Some(SocketMessage::Binary(bytes)) = message.map_err(failure)? { received[1] |= bytes.len() == 160 && bytes.iter().all(|sample| (i32::from(mulaw_decode(*sample)) - 4000).abs() < 300); } }
            }
        }
        for id in &ids { controller.execute(credentials.clone(), None, Action::Hangup(id.clone())).await?; }
        controller.execute(credentials, None, Action::DestroyBridge(bridge)).await?;
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    http.abort();
    assert!(http.await.is_err());
    result?
}
