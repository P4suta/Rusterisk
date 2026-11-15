use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ChannelId, StoredRecording};
use rusterisk_config::Config;
use rusterisk_media::Frame;
use rusterisk_server::{http::router, storage::StorageWorker, ControlError, Controller};
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
async fn json<T: serde::de::DeserializeOwned>(response: Response) -> Result<T, ControlError> {
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(failure)?
        .to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn stored_recording_routes_require_authentication_and_protect_writes_from_read_only_users(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let mut saved = storage
        .record(ChannelId::try_from("saved".to_owned())?, 8000)
        .await
        .map_err(failure)?;
    saved
        .try_write(Frame::new(&[1234; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    saved.finish().await.map_err(failure)?;
    let pending = storage
        .record(ChannelId::try_from("pending".to_owned())?, 8000)
        .await
        .map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=password\n[reader]\ntype=user\npassword=password\nread_only=yes\n", "", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.with_storage(storage.clone())?.run());
    let result = async {
        let app = router(controller.clone());
        assert_eq!(
            app.clone()
                .oneshot(request("GET", "/ari/recordings/stored", "invalid")?)
                .await
                .map_err(failure)?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .clone()
            .oneshot(request("GET", "/ari/recordings/stored", "reader")?)
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let saved: Vec<StoredRecording> = json(response).await?;
        assert_eq!(
            saved,
            [StoredRecording {
                name: "saved".into(),
                format: "wav".into()
            }]
        );
        let response = app
            .clone()
            .oneshot(request("GET", "/ari/recordings/stored/saved", "reader")?)
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json::<StoredRecording>(response).await?.name, "saved");
        for name in ["missing", "pending"] {
            assert_eq!(
                app.clone()
                    .oneshot(request(
                        "GET",
                        &format!("/ari/recordings/stored/{name}"),
                        "reader"
                    )?)
                    .await
                    .map_err(failure)?
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            app.clone()
                .oneshot(request("DELETE", "/ari/recordings/stored/saved", "reader")?)
                .await
                .map_err(failure)?
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request("DELETE", "/ari/recordings/stored/saved", "app")?)
                .await
                .map_err(failure)?
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.clone()
                .oneshot(request("GET", "/ari/recordings/stored/saved", "app")?)
                .await
                .map_err(failure)?
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.oneshot(request("DELETE", "/ari/recordings/stored/saved", "app")?)
                .await
                .map_err(failure)?
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(directory.path().join("recordings/pending.partial").exists());
        Ok(())
    }
    .await;
    pending.cancel().await.map_err(failure)?;
    controller.shutdown();
    runner.await.map_err(failure)??;
    worker.close().await.map_err(failure)?;
    result
}
