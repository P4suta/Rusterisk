use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ChannelId, EventKind, Playback, PlaybackState};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget,
    Milestone, PlaybackCompletion, Signal,
};
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn request(method: &str, path: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method(method)
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
async fn playback(response: Response) -> Result<Playback, ControlError> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(failure)?
        .to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn http_playback_returns_created_location_and_routes_controls_to_the_owned_task(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut subscription = controller.subscribe(Credentials::new("app".into(), "password".into())?, None, "app".into()).await?;
        let key = controller.incoming(IncomingCall { id: ChannelId::try_from("phone".to_owned())?, endpoint: "phone".into(), caller: "100".into(), context: "internal".into(), extension: "100".into(), target: IncomingTarget::Stasis { application: "app".into(), arguments: Vec::new() } }).await?;
        controller.signal(Signal::Milestone { key: key.clone(), milestone: Milestone::Answered }).await?;
        loop {
            let event = subscription.events.recv().await.ok_or(ControlError::Capacity)?;
            if matches!(event.kind, EventKind::ChannelStateChange { channel } if channel.state == rusterisk_api::ChannelState::Up) { break; }
        }
        let app = router(controller.clone());
        for query in ["", "?media=", "?media=sound:../secret", "?media=sound:welcome&lang=..", "?media=sound:welcome&lang=ja%2Fen"] {
            assert_eq!(app.clone().oneshot(request("POST", &format!("/ari/channels/phone/play{query}"))?).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST);
        }
        assert!(effects.try_recv().is_err());
        let response = app.clone().oneshot(request("POST", "/ari/channels/phone/play/fixed?media=sound:welcome&lang=ja")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers().get("Location").ok_or(ControlError::Capacity)?, "/ari/playbacks/fixed");
        let accepted = playback(response).await?;
        assert_eq!(accepted.language, "ja");
        assert_eq!(accepted.state, PlaybackState::Queued);
        assert_eq!(accepted.target_uri, "channel:phone");
        let Effect::PlayMedia { token, language, .. } = effects.recv().await.ok_or(ControlError::Capacity)? else { return Err(ControlError::Unsupported); };
        assert_eq!(language, "ja");
        let response = app.clone().oneshot(request("GET", "/ari/playbacks/fixed")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(playback(response).await?.id, accepted.id);
        assert_eq!(app.clone().oneshot(request("POST", "/ari/playbacks/fixed/control?operation=pause")?).await.map_err(failure)?.status(), StatusCode::CONFLICT);
        controller.signal(Signal::PlaybackStarted { key: key.clone(), token }).await?;
        while !matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::PlaybackStarted { .. }) {}
        for operation in ["pause", "unpause"] {
            assert_eq!(app.clone().oneshot(request("POST", &format!("/ari/playbacks/fixed/control?operation={operation}"))?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
            assert!(matches!(effects.recv().await, Some(Effect::ControlPlayback { .. })));
        }
        for (operation, expected) in [("restart", rusterisk_server::media::PlaybackControl::Restart), ("forward", rusterisk_server::media::PlaybackControl::Forward), ("reverse", rusterisk_server::media::PlaybackControl::Reverse)] {
            let status = app.clone().oneshot(request("POST", &format!("/ari/playbacks/fixed/control?operation={operation}"))?).await.map_err(failure)?.status();
            if status != StatusCode::NO_CONTENT { return Err(failure(format!("{operation} returned {status}"))); }
            if !matches!(effects.recv().await, Some(Effect::ControlPlayback { token: observed, operation }) if observed == token && operation == expected) { return Err(failure("incorrect playback control effect")); }
        }
        assert_eq!(app.clone().oneshot(request("POST", "/ari/playbacks/fixed/control?operation=invalid")?).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app.clone().oneshot(request("DELETE", "/ari/playbacks/fixed")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        assert!(matches!(effects.recv().await, Some(Effect::ControlPlayback { operation: rusterisk_server::media::PlaybackControl::Stop, .. })));
        controller.signal(Signal::PlaybackFinished { key, token, completion: PlaybackCompletion::Completed }).await?;
        while !matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::PlaybackFinished { .. }) {}
        assert_eq!(app.clone().oneshot(request("GET", "/ari/playbacks/fixed")?).await.map_err(failure)?.status(), StatusCode::NOT_FOUND);
        let response = app.clone().oneshot(request("POST", "/ari/channels/phone/play?media=recording:call-1")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get("Location").ok_or(ControlError::Capacity)?.to_str().map_err(failure)?.to_owned();
        let accepted = playback(response).await?;
        assert_eq!(location, format!("/ari/playbacks/{}", accepted.id));
        assert_eq!(app.clone().oneshot(request("POST", "/ari/bridges?bridgeId=mix")?).await.map_err(failure)?.status(), StatusCode::OK);
        let response = app.clone().oneshot(request("POST", "/ari/bridges/mix/play/bridge-play?media=sound:welcome&lang=ja_JP")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::CREATED);
        let accepted = playback(response).await?;
        assert_eq!(accepted.target_uri, "bridge:mix");
        assert_eq!(accepted.language, "ja_JP");
        assert!(matches!(effects.recv().await, Some(Effect::PlayMedia { language, .. }) if language == "en"));
        assert!(matches!(effects.recv().await, Some(Effect::PlayMedia { owner: rusterisk_server::MediaOwner::Bridge { .. }, language, .. }) if language == "ja_JP"));
        let response = app.clone().oneshot(request("POST", "/ari/bridges/mix/play?media=recording:call-1")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(playback(response).await?.target_uri, "bridge:mix");
        assert!(effects.try_recv().is_err());
        Ok(())
    }).await.map_err(failure)?;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
