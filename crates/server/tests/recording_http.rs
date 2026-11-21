use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ChannelId, EventKind, LiveRecording, RecordingState};
use rusterisk_config::Config;
use rusterisk_server::media::MediaHub;
use rusterisk_server::signaling::Signaling;
use rusterisk_server::storage::StorageWorker;
use rusterisk_server::{http::router, Action, ControlError, Controller, Credentials};
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn request(method: &str, path: &str, user: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:password"))
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}
async fn recording(response: Response) -> Result<LiveRecording, ControlError> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(failure)?
        .to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}
async fn event(
    events: &mut tokio::sync::mpsc::Receiver<rusterisk_api::Event>,
) -> Result<EventKind, ControlError> {
    Ok(tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .map_err(failure)?
        .ok_or(ControlError::Capacity)?
        .kind)
}

#[tokio::test]
async fn http_recording_admission_reports_conflicts_and_preserves_pause_stop_and_discard_controls(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut original =
        rusterisk_storage::Recorder::create(directory.path(), "taken", 8000).map_err(failure)?;
    original
        .write(rusterisk_media::Frame::new(&[1234; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    original.finish().map_err(failure)?;
    let original_audio =
        std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=password\n[reader]\ntype=user\npassword=password\nread_only=yes\n", "", "", "").map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices)
        .await?
        .with_storage(storage.clone());
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result: Result<(), ControlError> = async {
        let mut subscription = controller.subscribe(Credentials::new("app".into(), "password".into())?, None, "app".into()).await?;
        controller.execute(Credentials::new("app".into(), "password".into())?, None, Action::CreateBridge { id: ChannelId::try_from("mix".to_owned())?, name: String::new() }).await?;
        event(&mut subscription.events).await?;
        let app = router(controller.clone());
        for (query, expected) in [("", StatusCode::BAD_REQUEST), ("?name=x", StatusCode::BAD_REQUEST), ("?name=x&format=invalid", StatusCode::UNPROCESSABLE_ENTITY), ("?name=x&format=wav&maxDurationSeconds=-1", StatusCode::BAD_REQUEST), ("?name=x&format=wav&beep=invalid", StatusCode::BAD_REQUEST), ("?name=x&format=wav&ifExists=invalid", StatusCode::BAD_REQUEST), ("?name=x&format=wav&maxSilenceSeconds=2147483648", StatusCode::BAD_REQUEST)] {
            let response = app.clone().oneshot(request("POST", &format!("/ari/bridges/mix/record{query}"), "app")?).await.map_err(failure)?;
            assert_eq!(response.status(), expected);
        }
        let conflict = app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=taken&format=wav", "app")?).await.map_err(failure)?;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFailed { .. }));
        let accepted = app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=saved&format=wav", "app")?).await.map_err(failure)?;
        assert_eq!(accepted.status(), StatusCode::CREATED);
        assert_eq!(accepted.headers().get("Location").ok_or(ControlError::Capacity)?, "/ari/recordings/live/saved");
        assert_eq!(recording(accepted).await?.state, RecordingState::Queued);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { .. }));
        for (method, operation, expected) in [("POST", "pause", RecordingState::Paused), ("POST", "mute", RecordingState::Paused), ("DELETE", "pause", RecordingState::Recording), ("DELETE", "mute", RecordingState::Recording)] {
            assert_eq!(app.clone().oneshot(request(method, &format!("/ari/recordings/live/saved/{operation}"), "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
            let response = app.clone().oneshot(request("GET", "/ari/recordings/live/saved", "reader")?).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(recording(response).await?.state, expected);
        }
        assert_eq!(app.clone().oneshot(request("POST", "/ari/recordings/live/saved/mute", "reader")?).await.map_err(failure)?.status(), StatusCode::FORBIDDEN);
        assert_eq!(app.clone().oneshot(request("POST", "/ari/recordings/live/saved/stop", "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFinished { recording } if recording.state == RecordingState::Done));
        assert_eq!(app.clone().oneshot(request("GET", "/ari/recordings/live/saved", "app")?).await.map_err(failure)?.status(), StatusCode::NOT_FOUND);
        assert!(storage.recordings().await.map_err(failure)?.iter().any(|recording| recording.name == "saved"));
        assert_eq!(app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=discarded&format=wav", "app")?).await.map_err(failure)?.status(), StatusCode::CREATED);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { .. }));
        assert_eq!(app.clone().oneshot(request("DELETE", "/ari/recordings/live/discarded", "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFinished { recording } if recording.state == RecordingState::Canceled));
        assert!(!storage.recordings().await.map_err(failure)?.iter().any(|recording| recording.name == "discarded"));
        for (method, suffix, state) in [("DELETE", "", RecordingState::Canceled), ("POST", "/stop", RecordingState::Done)] {
            let response = app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=taken&format=wav&ifExists=append", "app")?).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::CREATED);
            assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { .. }));
            assert_eq!(std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?, original_audio);
            assert_eq!(app.clone().oneshot(request(method, &format!("/ari/recordings/live/taken{suffix}"), "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
            assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFinished { recording } if recording.state == state));
            let audio = std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?;
            assert_eq!(&audio[44..original_audio.len()], &original_audio[44..]);
            if state == RecordingState::Canceled { assert_eq!(audio, original_audio); }
            assert!(!directory.path().join("recordings/taken.append").exists());
        }
        let original_audio = std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?;
        for (method, suffix, state) in [("DELETE", "", RecordingState::Canceled), ("POST", "/stop", RecordingState::Done)] {
            let response = app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=taken&format=wav&ifExists=OVERWRITE", "app")?).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::CREATED);
            assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { .. }));
            assert_eq!(std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?, original_audio);
            assert_eq!(app.clone().oneshot(request(method, &format!("/ari/recordings/live/taken{suffix}"), "app")?).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
            assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFinished { recording } if recording.state == state));
            let audio = std::fs::read(directory.path().join("recordings/taken.wav")).map_err(failure)?;
            if state == RecordingState::Canceled { assert_eq!(audio, original_audio); } else { assert_ne!(audio, original_audio); }
            assert!(!directory.path().join("recordings/taken.replacement").exists());
        }
        let response = app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=limited&format=wav&maxDurationSeconds=1", "app")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { .. }));
        let finished = tokio::time::timeout(Duration::from_secs(5), subscription.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
        assert!(matches!(finished.kind, EventKind::RecordingFinished { recording } if recording.name == "limited" && recording.state == RecordingState::Done));
        assert_eq!(app.clone().oneshot(request("GET", "/ari/recordings/live/limited", "app")?).await.map_err(failure)?.status(), StatusCode::NOT_FOUND);
        assert!(directory.path().join("recordings/limited.wav").exists());
        assert_eq!(app.clone().oneshot(request("POST", "/ari/bridges/mix/record?name=invalid&format=wav&terminateOn=0", "app")?).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST);
        for body in [r#"{"terminateOn":"0"}"#, r#"{"maxSilenceSeconds":-1}"#] {
            let mut req = request("POST", "/ari/bridges/mix/record?name=json-options&format=wav&terminateOn=none&maxSilenceSeconds=1", "app")?;
            *req.body_mut() = Body::from(body);
            assert_eq!(app.clone().oneshot(req).await.map_err(failure)?.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    worker.close().await.map_err(failure)?;
    result
}
