use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::Config;
use rusterisk_core::AudioGate;
use rusterisk_server::{
    http::router, Action, ControlError, Controller, Credentials, Effect, IncomingCall, Milestone,
    Signal,
};
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn http_request(method: &str, path: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("app:test-password")
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}

#[tokio::test]
async fn incoming_admission_publishes_stasis_before_reply_and_http_answer_waits_for_signaling(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut subscription = controller.subscribe(credentials.clone(), None, "app".into()).await?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        let key = controller.incoming(IncomingCall {
            id: id.clone(), endpoint: "phone".into(), caller: "100".into(), context: "internal".into(), extension: "200".into(), app: "app".into(), args: Vec::new(),
        }).await?;
        let first = subscription.events.try_recv().map_err(failure)?;
        let second = subscription.events.try_recv().map_err(failure)?;
        assert!(matches!(first.kind, EventKind::ChannelCreated { .. }));
        assert!(matches!(second.kind, EventKind::StasisStart { channel, .. } if channel.state == ChannelState::Ring));
        let request = Request::builder().method("POST").uri("/ari/channels/incoming/answer").header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
        let response = router(controller.clone()).oneshot(request).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(matches!(effects.try_recv().map_err(failure)?, Effect::Answer { key: answered } if answered == key));
        let snapshot = controller.execute(credentials.clone(), None, Action::GetChannel(id.clone())).await?.body.ok_or(ControlError::Capacity)?;
        assert_eq!(snapshot["state"], "Ring");
        controller.signal(Signal::Milestone { key, milestone: Milestone::Answered }).await?;
        let changed = tokio::time::timeout(Duration::from_secs(1), subscription.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
        assert!(matches!(changed.kind, EventKind::ChannelStateChange { channel } if channel.state == ChannelState::Up));
        for (method, path, expected) in [
            ("POST", "/ari/channels/incoming/hold", AudioGate::Closed),
            ("DELETE", "/ari/channels/incoming/hold", AudioGate::Open),
            ("POST", "/ari/channels/incoming/mute?direction=in", AudioGate::IncomingMuted),
            ("POST", "/ari/channels/incoming/mute?direction=both", AudioGate::Closed),
            ("DELETE", "/ari/channels/incoming/mute?direction=out", AudioGate::IncomingMuted),
            ("DELETE", "/ari/channels/incoming/mute?direction=in", AudioGate::Open),
        ] {
            let response = router(controller.clone()).oneshot(http_request(method, path)?).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
            assert!(matches!(effects.try_recv().map_err(failure)?, Effect::Gate { gate, .. } if gate == expected));
        }
        for path in ["/ari/channels/incoming/mute", "/ari/channels/incoming/mute?direction=unknown"] {
            let response = router(controller.clone()).oneshot(http_request("POST", path)?).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }
        let app = router(controller.clone());
        let changed = app.clone().oneshot(http_request("POST", "/ari/channels/incoming/variable?variable=TEST&value=call%2Bbridge")?).await.map_err(failure)?;
        assert_eq!(changed.status(), StatusCode::NO_CONTENT);
        let variable = app.clone().oneshot(http_request("GET", "/ari/channels/incoming/variable?variable=TEST")?).await.map_err(failure)?;
        assert_eq!(variable.status(), StatusCode::OK);
        let body = variable.into_body().collect().await.map_err(failure)?.to_bytes();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body)?, serde_json::json!({"value": "call+bridge"}));
        assert_eq!(app.clone().oneshot(http_request("POST", "/ari/channels/incoming/variable?variable=TEST")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        assert_eq!(app.oneshot(http_request("GET", "/ari/channels/incoming/variable?variable=TEST")?).await.map_err(failure)?.status(), StatusCode::NOT_FOUND);
        controller.execute(credentials, None, Action::Hangup(id)).await?;
        Ok(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
