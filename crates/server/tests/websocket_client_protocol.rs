use axum::{extract::ws::WebSocketUpgrade, http::HeaderValue};
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub,
    media_websocket::{MediaSocket, SocketMessage},
    signaling::Signaling,
    Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::{future::IntoFuture, time::Duration};
use tokio::sync::mpsc;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn refused_protocol(wrong: bool) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (sender, mut sockets) = mpsc::channel(4);
    let app = axum::Router::new().route(
        "/media",
        axum::routing::get(move |upgrade: WebSocketUpgrade| {
            let sender = sender.clone();
            async move {
                let mut response = upgrade
                    .protocols(["media"])
                    .on_upgrade(async move |socket| {
                        assert!(sender.try_send(socket).is_ok());
                    });
                if wrong {
                    response
                        .headers_mut()
                        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("other"));
                } else {
                    response.headers_mut().remove("Sec-WebSocket-Protocol");
                }
                response
            }
        }),
    );
    let http = tokio::spawn(axum::serve(listener, app).into_future());
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?
        .with_websocket_configuration("", &format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{address}/media?token=private-query\nprotocols=media\nusername=private-username\npassword=private-password\nconnection_timeout=500\nreconnect_interval=100\nreconnect_attempts=2\n")).map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = Wait::Ready.bounded(async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _subscription = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let id = ChannelId::try_from("refused".to_owned())?;
        let started = tokio::time::Instant::now();
        let result = controller.execute(credentials.clone(), None, Action::ExternalWebSocketClient { id: id.clone(), app: "calls".into(), client: "agent".into(), format: MediaFormat::Pcmu, control_format: Some(MediaControlFormat::Json), direction: MediaDirection::Both, auto_answer: true, data: String::new() }).await;
        assert!(matches!(result, Err(ControlError::Adapter(message)) if message == "WebSocket connection failed"));
        assert!(started.elapsed() >= Duration::from_millis(100));
        for _ in 0..2 {
            let mut socket = sockets.recv().await.ok_or(ControlError::Capacity)?;
            assert!(matches!(MediaSocket::receive(&mut socket).await, Ok(None | Some(SocketMessage::Close)) | Err(_)));
        }
        assert!(sockets.try_recv().is_err());
        assert!(matches!(controller.execute(credentials, None, Action::GetChannel(id)).await, Err(ControlError::NotFound(_))));
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    http.abort();
    assert!(http.await.is_err());
    result?
}

#[tokio::test]
async fn absent_media_negotiation_exhausts_the_configured_attempts_without_attaching_audio(
) -> Result<(), ControlError> {
    refused_protocol(false).await
}

#[tokio::test]
async fn wrong_media_negotiation_exhausts_the_configured_attempts_without_attaching_audio(
) -> Result<(), ControlError> {
    refused_protocol(true).await
}
