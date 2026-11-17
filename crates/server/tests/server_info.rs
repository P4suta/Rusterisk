use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::AsteriskInfo;
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller};
use tower::ServiceExt;

fn request(path: &str, authenticated: bool) -> Result<Request<Body>, ControlError> {
    let mut request = Request::builder().uri(path);
    if authenticated {
        request = request.header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("reader:password")
            ),
        );
    }
    request
        .body(Body::empty())
        .map_err(|error| ControlError::Adapter(error.to_string()))
}

#[tokio::test]
async fn info_filters_are_case_insensitive_and_keep_process_identity_and_startup_time(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[reader]\ntype=user\npassword=password\nread_only=yes\n",
        "",
        "",
        "",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        let ping = app
            .clone()
            .oneshot(request("/ari/asterisk/ping", true)?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
        assert_eq!(ping.status(), StatusCode::OK);
        let bytes = ping
            .into_body()
            .collect()
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .to_bytes();
        let ping: rusterisk_api::AsteriskPing = serde_json::from_slice(&bytes)?;
        assert_eq!(ping.ping, "pong");
        assert!(ping.timestamp.ends_with("+00:00"));
        assert!(ping.timestamp.parse::<jiff::Timestamp>().is_ok());
        let denied = app
            .clone()
            .oneshot(request("/ari/asterisk/info", false)?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let mut identity = None;
        let mut startup = None;
        for (query, expected) in [
            ("", [true, true, true, true]),
            ("?only=SYSTEM,status", [false, true, false, true]),
            ("?only=unknown", [false; 4]),
            ("?only=build,build", [true, false, false, false]),
        ] {
            let response = app
                .clone()
                .oneshot(request(&format!("/ari/asterisk/info{query}"), true)?)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?;
            assert_eq!(response.status(), StatusCode::OK, "{query}");
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .to_bytes();
            let info: AsteriskInfo = serde_json::from_slice(&bytes)?;
            assert_eq!(
                [
                    info.build.is_some(),
                    info.system.is_some(),
                    info.config.is_some(),
                    info.status.is_some()
                ],
                expected
            );
            if let Some(system) = info.system {
                assert_eq!(system.version, env!("CARGO_PKG_VERSION"));
                assert!(uuid::Uuid::parse_str(&system.entity_id).is_ok());
                if let Some(previous) = identity.replace(system.entity_id.clone()) {
                    assert_eq!(previous, system.entity_id);
                }
            }
            if let Some(status) = info.status {
                assert!(status.startup_time.parse::<jiff::Timestamp>().is_ok());
                assert!(status.startup_time.ends_with("+00:00"));
                assert_eq!(status.last_reload_time, status.startup_time);
                if let Some(previous) = startup.replace(status.startup_time.clone()) {
                    assert_eq!(previous, status.startup_time);
                }
            }
            if let Some(config) = info.config {
                assert_eq!(config.max_channels, Some(4));
                assert!(config.max_load.is_none());
                assert!(config.max_open_files.is_none());
            }
            if let Some(build) = info.build {
                assert_eq!(build.os, std::env::consts::OS);
                assert_eq!(build.machine, std::env::consts::ARCH);
            }
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    runner
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}
