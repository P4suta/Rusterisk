use axum::{body::Body, http::Request};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{ApiError, Sound};
use rusterisk_config::Config;
use rusterisk_media::Frame;
use rusterisk_server::{http::router, storage::StorageWorker, ControlError, Controller};
use rusterisk_storage::Recorder;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn request(path: &str, user: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
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

#[tokio::test]
async fn sound_routes_authenticate_readers_filter_formats_and_preserve_nested_sound_names(
) -> Result<(), ControlError> {
    let root = tempfile::tempdir().map_err(failure)?;
    let staging = tempfile::tempdir().map_err(failure)?;
    let mut audio = Recorder::create(staging.path(), "one", 8000).map_err(failure)?;
    audio
        .write(Frame::new(&[1234; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    std::fs::create_dir_all(root.path().join("sounds/en/digits")).map_err(failure)?;
    std::fs::rename(
        audio.finish().map_err(failure)?,
        root.path().join("sounds/en/digits/one.wav"),
    )
    .map_err(failure)?;
    let (storage, worker) = StorageWorker::start(root.path().to_owned())
        .await
        .map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[reader]\ntype=user\npassword=password\nread_only=yes\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.with_storage(storage)?.run());
    let result = async {
        let app = router(controller.clone());
        assert_eq!(
            app.clone()
                .oneshot(request("/ari/sounds", "invalid")?)
                .await
                .map_err(failure)?
                .status(),
            401
        );
        for path in [
            "/ari/sounds",
            "/ari/sounds?lang=en&format=slin",
            "/ari/sounds?lang=&format=",
        ] {
            let response = app
                .clone()
                .oneshot(request(path, "reader")?)
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), 200, "{path}");
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(failure)?
                .to_bytes();
            let sounds: Vec<Sound> = serde_json::from_slice(&bytes)?;
            assert_eq!(sounds.len(), 1);
            assert_eq!(sounds[0].id, "digits/one");
            assert_eq!(sounds[0].text, None);
            assert_eq!(sounds[0].formats[0].format, "slin");
        }
        let response = app
            .clone()
            .oneshot(request("/ari/sounds/digits%2Fone", "reader")?)
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), 200);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(failure)?
            .to_bytes();
        assert_eq!(serde_json::from_slice::<Sound>(&bytes)?.id, "digits/one");
        for (path, expected) in [
            (
                "/ari/sounds?format=wav",
                "No sounds found that matched the query",
            ),
            (
                "/ari/sounds?lang=JA",
                "No sounds found that matched the query",
            ),
            ("/ari/sounds/missing", "Sound not found"),
        ] {
            let response = app
                .clone()
                .oneshot(request(path, "reader")?)
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), 404, "{path}");
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(failure)?
                .to_bytes();
            assert_eq!(
                serde_json::from_slice::<ApiError>(&bytes)?.message,
                expected
            );
        }
        for path in [
            "/ari/sounds/%2e%2e%2Fsecret",
            "/ari/sounds/%252e%252e%252fsecret",
            "/ari/sounds/%ff",
            "/ari/sounds/a%5cb",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(path, "reader")?)
                    .await
                    .map_err(failure)?
                    .status(),
                400,
                "{path}"
            );
        }
        Ok::<(), ControlError>(())
    }
    .await;
    controller.shutdown();
    let joined = runner.await.map_err(failure)?;
    let cleanup = worker.close().await.map_err(failure);
    result.and(joined).and(cleanup)
}
