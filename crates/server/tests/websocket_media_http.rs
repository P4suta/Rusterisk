use axum::http::HeaderValue;
use rusterisk_api::{Channel, ChannelId, EventKind, MediaEvent};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router_with_media,
    media::MediaHub,
    media_websocket::{MediaSocket, SocketMessage},
    signaling::Signaling,
    Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::future::IntoFuture;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn incoming_media_upgrades_bind_one_channel_and_clean_up_after_disconnect(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[general]\nallowed_origins=https://accepted.example\n[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?
        .with_websocket_configuration("[global]\ncontrol_message_format=json\n", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling =
        Signaling::bind(config, controller.clone(), runner, media.clone(), notices).await?;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let http = tokio::spawn(
        axum::serve(listener, router_with_media(controller.clone(), media)).into_future(),
    );
    let result = Wait::Ready.bounded(async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut subscription = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let client = reqwest::Client::new();
        let response = client.post(format!("http://{address}/ari/channels/externalMedia?app=calls&transport=websocket&encapsulation=none&connection_type=server&format=ulaw&channelId=external"))
            .basic_auth("app", Some("test-password")).send().await.map_err(failure)?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let channel: Channel = response.json().await.map_err(failure)?;
        let connection = channel.channelvars.ok_or(ControlError::Capacity)?.remove("MEDIA_WEBSOCKET_CONNECTION_ID").ok_or(ControlError::Capacity)?;
        let mut request = format!("ws://{address}/media/{connection}").into_client_request().map_err(failure)?;
        request.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static("media"));
        request.headers_mut().insert("Origin", HeaderValue::from_static("https://refused.example"));
        let refused = tokio_tungstenite::connect_async(request.clone()).await;
        assert!(matches!(refused, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 403));
        request.headers_mut().insert("Origin", HeaderValue::from_static("https://accepted.example"));
        for protocol in ["unsupported".to_owned(), format!("media,{}", "x".repeat(251))] {
            let mut rejected = request.clone();
            rejected.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_str(&protocol).map_err(failure)?);
            if !matches!(tokio_tungstenite::connect_async(rejected).await, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 400) {
                return Err(ControlError::Invalid("Expected media protocol rejection"));
            }
        }
        let (mut peer, response) = tokio_tungstenite::connect_async(request.clone()).await.map_err(failure)?;
        assert_eq!(response.headers()["Sec-WebSocket-Protocol"], "media");
        let duplicate = tokio_tungstenite::connect_async(request).await;
        assert!(matches!(duplicate, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 409));
        let Some(SocketMessage::Text(start)) = MediaSocket::receive(&mut peer).await.map_err(failure)? else { return Err(ControlError::Unsupported); };
        assert!(matches!(serde_json::from_str::<MediaEvent>(&start)?, MediaEvent::MediaStart { channel_id, optimal_frame_size: 160, .. } if channel_id.as_str() == "external"));
        loop { if matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::StasisStart { .. }) { break; } }
        MediaSocket::send(&mut peer, SocketMessage::Binary(vec![0xff; 160].into())).await.map_err(failure)?;
        loop {
            let outcome = controller.execute(credentials.clone(), None, Action::GetVariable { channel: ChannelId::try_from("external".to_owned())?, variable: "RUSTERISK_MEDIA_CONNECTED".into() }).await?;
            if outcome.body.as_ref().is_some_and(|body| body["value"] == "1") { break; }

        }
        MediaSocket::send(&mut peer, SocketMessage::Close).await.map_err(failure)?;
        loop { match MediaSocket::receive(&mut peer).await.map_err(failure)? { Some(SocketMessage::Close) => break, Some(SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_)) => {}, Some(SocketMessage::Ping(bytes)) => MediaSocket::send(&mut peer, SocketMessage::Pong(bytes)).await.map_err(failure)?, None => return Err(ControlError::Unsupported) } }
        loop { if matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::StasisEnd { .. }) { break; } }
        assert!(matches!(controller.execute(credentials, None, Action::GetChannel(ChannelId::try_from("external".to_owned())?)).await, Err(ControlError::NotFound(_))));
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    http.abort();
    assert!(http.await.is_err());
    result?
}
