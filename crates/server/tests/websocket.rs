use axum::http::HeaderValue;
use futures_util::StreamExt;
use rusterisk_api::{ChannelId, Event, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{http::router, Action, ControlError, Controller, Credentials};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn websocket_upgrade_registers_the_app_before_returning_and_streams_real_events(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let app = router(controller.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                if stopped.await.is_err() {
                    tracing::debug!("HTTP shutdown sender disconnected");
                }
            })
            .await
    });
    let mut request = format!("ws://{address}/ari/events?app=app")
        .into_client_request()
        .map_err(failure)?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_static("Basic YXBwOnRlc3QtcGFzc3dvcmQ="),
    );
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("ari"));
    let result = async {
        let (mut socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(failure)?;
        assert_eq!(response.headers()["Sec-WebSocket-Protocol"], "ari");
        controller
            .execute(
                Credentials::new("app".into(), "test-password".into())?,
                None,
                Action::CreateBridge {
                    id: ChannelId::try_from("conversation".to_owned())?,
                    name: "Conversation".into(),
                },
            )
            .await?;
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?
            .map_err(failure)?;
        let event: Event = serde_json::from_str(message.to_text().map_err(failure)?)?;
        assert!(matches!(event.kind, EventKind::BridgeCreated { bridge } if bridge.id.as_str() == "conversation"));
        socket.close(None).await.map_err(failure)?;
        Ok(())
    }
    .await;
    controller.shutdown();
    if stop.send(()).is_err() {
        tracing::debug!("HTTP test server already stopped");
    }
    runner.await.map_err(failure)??;
    server.await.map_err(failure)?.map_err(failure)?;
    result
}
