use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use http_body_util::BodyExt;
use rusterisk_api::{Channel, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller, Credentials};
use serde_json::Value;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: &'static str,
) -> Result<Response, ControlError> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Basic YXBwOnRlc3QtcGFzc3dvcmQ=")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(failure)?;
    app.clone().oneshot(request).await.map_err(failure)
}

#[tokio::test]
async fn channel_creation_installs_validated_body_variables_before_events_and_effects(
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
    let task = tokio::spawn(runner.run());
    let result = async {
        let mut subscription = controller.subscribe(Credentials::new("app".into(), "test-password".into())?, None, "calls".into()).await?;
        let app = router(controller.clone());
        let path = "/ari/channels?endpoint=PJSIP/phone&app=calls&channelId=initial&callerId=100";
        for invalid in [r#"{"variables":{"FIRST":"one","LAST":"bad\nvalue"}}"#, r#"{"variables":{"FIRST":"one","SECOND":42}}"#] {
            assert_eq!(request(&app, "POST", path, invalid).await?.status(), StatusCode::BAD_REQUEST);
            assert_eq!(request(&app, "GET", "/ari/channels/initial", "").await?.status(), StatusCode::NOT_FOUND);
            assert!(subscription.events.try_recv().is_err());
        }
        let response = request(&app, "POST", path, r#"{"variables":{"FIRST":"one","SECOND":"","CALLERID(name)":"Alice"}}"#).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let channel: Channel = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(channel.caller.name, "Alice");
        assert_eq!(channel.caller.number, "100");
        let event = subscription.events.try_recv().map_err(failure)?;
        assert!(matches!(event.kind, EventKind::ChannelCreated { channel } if channel.caller.name == "Alice"));
        for (variable, expected) in [("FIRST", "one"), ("SECOND", "")] {
            let response = request(&app, "GET", &format!("/ari/channels/initial/variable?variable={variable}"), "").await?;
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
            assert_eq!(value, serde_json::json!({"value": expected}));
        }
        Ok::<(), ControlError>(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
