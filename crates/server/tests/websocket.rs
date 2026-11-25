use axum::http::HeaderValue;
use futures_util::StreamExt;
use rusterisk_api::{ChannelId, Event, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{http::router, Action, ControlError, Controller, Credentials};
use rusterisk_test_support::Wait;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::frame::coding::CloseCode, protocol::CloseFrame, Message,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!(
            "{primary}; WebSocket test cleanup failed: {cleanup}"
        ))),
    }
}

async fn joined<E: std::fmt::Display>(
    mut task: tokio::task::JoinHandle<Result<(), E>>,
) -> Result<(), ControlError> {
    match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            let result = Err(failure(primary));
            match task.await {
                Err(error) if error.is_cancelled() => result,
                Err(error) => combine(result, Err(failure(error))),
                Ok(Err(error)) => combine(result, Err(failure(error))),
                Ok(Ok(())) => result,
            }
        }
    }
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
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
    let result = async {
        let mut request = format!("ws://{address}/ari/events?app=app")
            .into_client_request()
            .map_err(failure)?;
        let mut authorization = HeaderValue::from_static("Basic YXBwOnRlc3QtcGFzc3dvcmQ=");
        authorization.set_sensitive(true);
        request.headers_mut().insert("Authorization", authorization);
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("ari"));
        let (mut socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(failure)?;
        if response.headers().get("Sec-WebSocket-Protocol") != Some(&HeaderValue::from_static("ari")) {
            return Err(ControlError::Invalid("Expected the ARI protocol"));
        }
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
        let message = Wait::Ready.bounded(socket.next())
            .await
            .map_err(failure)?
            .ok_or(ControlError::Capacity)?
            .map_err(failure)?;
        let event: Event = serde_json::from_str(message.to_text().map_err(failure)?)?;
        assert!(event.timestamp.parse::<jiff::Timestamp>().is_ok());
        let (_, fraction) = event.timestamp.rsplit_once('.').ok_or(ControlError::Invalid("Event timestamp has no millisecond fraction"))?;
        assert_eq!(fraction.len(), 4);
        assert!(fraction.ends_with('Z'));
        if !matches!(event.kind, EventKind::BridgeCreated { bridge } if bridge.id.as_str() == "conversation") {
            return Err(ControlError::Invalid("Expected the bridge event"));
        }
        let close = CloseFrame { code: CloseCode::Normal, reason: "Event consumer finished".into() };
        socket.close(Some(close.clone())).await.map_err(failure)?;
        let response = Wait::Ready.bounded(socket.next())
            .await.map_err(failure)?.ok_or(ControlError::Invalid("Close response missing"))?.map_err(failure)?;
        if response != Message::Close(Some(close)) {
            return Err(ControlError::Invalid("Close response did not match"));
        }
        if Wait::Ready.bounded(socket.next())
            .await.map_err(failure)?.transpose().map_err(failure)?.is_some() {
            return Err(ControlError::Invalid("WebSocket TCP connection remained open"));
        }
        match controller.execute(
            Credentials::new("app".into(), "test-password".into())?,
            None,
            Action::GetApplication("app".into()),
        ).await {
            Err(ControlError::NotFound(_)) => {},
            Err(error) => return Err(error),
            Ok(_) => return Err(ControlError::Invalid("WebSocket subscription remained registered")),
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    let result = combine(
        result,
        stop.send(())
            .map_err(|()| ControlError::Invalid("HTTP test server already stopped")),
    );
    let result = combine(result, joined(runner).await);
    combine(result, joined(server).await)
}
