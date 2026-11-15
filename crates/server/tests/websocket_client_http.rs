use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use rusterisk_api::{Channel, MediaControlFormat, MediaDirection};
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller, Credentials, Effect, Signal};
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn request(query: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method("POST")
        .uri(format!("/ari/channels/externalMedia?{query}"))
        .header("Authorization", "Basic YXBwOnRlc3QtcGFzc3dvcmQ=")
        .body(Body::empty())
        .map_err(failure)
}

#[tokio::test]
async fn client_http_creation_waits_for_attachment_and_resolves_the_global_format(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?
        .with_websocket_configuration("[global]\ncontrol_message_format=json\n", "[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://127.0.0.1:18080/media\nprotocols=media\nreconnect_attempts=1\n").map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let _subscription = controller.subscribe(Credentials::new("app".into(), "test-password".into())?, None, "calls".into()).await?;
        let app = router(controller.clone());
        let query = "app=calls&transport=websocket&encapsulation=none&format=ulaw&connection_type=client&channelId=external&direction=out&transport_data=n&data=media,agent";
        let refused = app.clone().oneshot(request(&format!("{query}&external_host=unknown"))?).await.map_err(failure)?;
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let error: serde_json::Value = serde_json::from_slice(&refused.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(error["message"], "external_host must be a valid websocket_client connection id.");
        let pending = tokio::spawn(app.oneshot(request(&format!("{query}&external_host=agent"))?));
        let Some(Effect::ConnectWebSocket { key, client, options, .. }) = effects.recv().await else { return Err(ControlError::Capacity); };
        assert_eq!(client.name, "agent");
        assert_eq!(options.control_format, MediaControlFormat::Json);
        assert_eq!(options.direction, MediaDirection::Out);
        assert!(!options.auto_answer);
        assert!(!pending.is_finished());
        controller.signal(Signal::WebSocketAdmitted { key, result: Ok(()) }).await?;
        let response = pending.await.map_err(failure)?.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let channel: Channel = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(channel.id.as_str(), "external");
        assert_eq!(channel.state, rusterisk_api::ChannelState::Down);
        assert_eq!(channel.channelvars.ok_or(ControlError::Capacity)?.get("MEDIA_WEBSOCKET_CONNECTION_ID").map(String::as_str), Some("agent"));
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    runner.await.map_err(failure)??;
    result?
}
