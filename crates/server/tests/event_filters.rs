use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{Application, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{http::router, Action, ControlError, Controller, Credentials};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn request(body: &str, username: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method("PUT")
        .uri("/ari/applications/calls/eventFilter")
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:test-password"))
            ),
        )
        .body(Body::from(body.to_owned()))
        .map_err(failure)
}

#[tokio::test]
async fn filter_updates_preserve_omitted_lists_and_block_events_before_delivery(
) -> Result<(), ControlError> {
    let configuration = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n[reader]\ntype=user\npassword=test-password\nread_only=yes\n", "", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(configuration, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut subscription = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let app = router(controller.clone());
        let response = app.clone().oneshot(request(r#"{"allowed":[{"type":"BridgeCreated"}],"disallowed":[{"type":"BridgeCreated"}]}"#, "app")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let filters: Application = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(filters.events_allowed[0].event_type, "BridgeCreated");
        assert_eq!(filters.events_disallowed[0].event_type, "BridgeCreated");
        let bridge = rusterisk_api::ChannelId::try_from("conversation".to_owned())?;
        controller.execute(credentials.clone(), None, Action::CreateBridge { id: bridge.clone(), name: "Conversation".into() }).await?;
        assert!(matches!(subscription.events.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        let response = app.clone().oneshot(request(r#"{"allowed":[{"type":"BridgeDestroyed"}]}"#, "app")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let filters: Application = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(filters.events_disallowed[0].event_type, "BridgeCreated");
        controller.execute(credentials.clone(), None, Action::DestroyBridge(bridge.clone())).await?;
        assert!(matches!(subscription.events.try_recv(), Ok(event) if matches!(event.kind, EventKind::BridgeDestroyed { .. })));
        let denied = app.clone().oneshot(request("{}", "reader")?).await.map_err(failure)?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        for body in [r#"{"allowed":null}"#, r#"{"allowed":[{}]}"#, r#"{"disallowed":[{"type":""}]}"#] {
            assert_eq!(app.clone().oneshot(request(body, "app")?).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST);
        }
        let reset = app.oneshot(request("", "app")?).await.map_err(failure)?;
        assert_eq!(reset.status(), StatusCode::OK);
        let filters: Application = serde_json::from_slice(&reset.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert!(filters.events_allowed.is_empty());
        assert!(filters.events_disallowed.is_empty());
        controller.execute(credentials, None, Action::CreateBridge { id: bridge, name: "Restored".into() }).await?;
        assert!(matches!(subscription.events.try_recv(), Ok(event) if matches!(event.kind, EventKind::BridgeCreated { .. })));
        Ok(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
