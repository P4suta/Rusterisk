use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::Config;
use rusterisk_core::DtmfDigit;
use rusterisk_server::{
    http::router, ControlError, Controller, Credentials, Effect, IncomingCall, Milestone, Signal,
};
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn request(path: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("app:password")
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}

#[tokio::test]
async fn http_digits_validate_state_timing_and_capacity_before_accepting_a_command(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 1);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "password".into())?;
        let mut subscription = controller.subscribe(credentials, None, "app".into()).await?;
        let key = controller.incoming(IncomingCall {
            id: ChannelId::try_from("call".to_owned())?,
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: rusterisk_server::IncomingTarget::Stasis {
                application: "app".into(),
                arguments: Vec::new(),
            },
        }).await?;
        let app = router(controller.clone());
        assert_eq!(app.clone().oneshot(request("/ari/channels/call/dtmf?dtmf=12")?).await.map_err(failure)?.status(), StatusCode::PRECONDITION_FAILED);
        controller.signal(Signal::DtmfReady { key: key.clone(), events: u16::MAX }).await?;
        controller.signal(Signal::Milestone { key, milestone: Milestone::Answered }).await?;
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = subscription.events.recv().await {
                if matches!(event.kind, EventKind::ChannelStateChange { channel } if channel.state == ChannelState::Up) { return Ok(()); }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        for invalid in ["", "?dtmf=", "?dtmf=x", "?dtmf=1&duration=0", "?dtmf=1&duration=5001", "?dtmf=1&before=-1", "?dtmf=1&between=bad", "?dtmf=1&after=4294967296"] {
            assert_eq!(app.clone().oneshot(request(&format!("/ari/channels/call/dtmf{invalid}"))?).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST, "{invalid}");
        }
        assert_eq!(app.clone().oneshot(request("/ari/channels/missing/dtmf?dtmf=1")?).await.map_err(failure)?.status(), StatusCode::NOT_FOUND);
        assert_eq!(app.clone().oneshot(request("/ari/channels/call/dtmf?dtmf=12")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        let Effect::Dtmf { mut command, .. } = effects.try_recv().map_err(failure)? else { return Err(ControlError::Capacity); };
        assert_eq!(command.sequence.advance(100).map_err(ControlError::from)?.ok_or(ControlError::Capacity)?.digit, DtmfDigit::One);
        assert!(command.sequence.advance(299).map_err(ControlError::from)?.is_none());
        let second = command.sequence.advance(300).map_err(ControlError::from)?.ok_or(ControlError::Capacity)?;
        assert_eq!(second.digit, DtmfDigit::Two);
        assert_eq!(second.duration_ms, 100);
        assert!(command.sequence.advance(400).map_err(ControlError::from)?.is_none());
        assert!(command.sequence.finished());
        assert_eq!(app.clone().oneshot(request("/ari/channels/call/dtmf?dtmf=%23%2A&before=20&between=30&duration=60&after=40")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        let Effect::Dtmf { mut command, .. } = effects.try_recv().map_err(failure)? else { return Err(ControlError::Capacity); };
        assert!(command.sequence.advance(10).map_err(ControlError::from)?.is_none());
        let first = command.sequence.advance(30).map_err(ControlError::from)?.ok_or(ControlError::Capacity)?;
        assert_eq!(first.digit, DtmfDigit::Hash);
        assert_eq!(first.duration_ms, 60);
        assert_eq!(command.sequence.advance(120).map_err(ControlError::from)?.ok_or(ControlError::Capacity)?.digit, DtmfDigit::Star);
        command.sequence.advance(219).map_err(ControlError::from)?;
        assert!(!command.sequence.finished());
        command.sequence.advance(220).map_err(ControlError::from)?;
        assert!(command.sequence.finished());
        for _ in 0..6 {
            assert_eq!(app.clone().oneshot(request("/ari/channels/call/dtmf?dtmf=1")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        }
        assert_eq!(app.oneshot(request("/ari/channels/call/dtmf?dtmf=1")?).await.map_err(failure)?.status(), StatusCode::SERVICE_UNAVAILABLE);
        Ok(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
