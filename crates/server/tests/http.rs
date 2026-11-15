use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller};
use tower::ServiceExt;

fn config() -> Result<Config, ControlError> {
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[general]\nallowed_origins=https://localhost\n[test]\ntype=user\npassword=password\n[reader]\ntype=user\npassword=password\nread_only=yes\n", "", "", "").map_err(|error| ControlError::Adapter(error.to_string()))
}

fn request(method: &str, path: &str, user: Option<&str>) -> Result<Request<Body>, ControlError> {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(user) = user {
        request = request.header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:password"))
            ),
        );
    }
    request
        .body(Body::empty())
        .map_err(|error| ControlError::Adapter(error.to_string()))
}

#[tokio::test]
async fn discovery_documents_use_the_requested_authority_and_refuse_invalid_hosts(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 1);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        for (path, version, base) in [
            ("/ari/api-docs/resources.json", "1.1", "/ari"),
            ("/ari/api-docs/channels.json", "1.1", "/ari"),
            ("/rusterisk/api-docs/resources.json", "1.2", "/rusterisk"),
            ("/rusterisk/api-docs/channels.json", "1.2", "/ari"),
        ] {
            let mut request = request("GET", path, Some("test"))?;
            request.headers_mut().insert(
                "Host",
                "pbx.example:8088".parse().map_err(
                    |error: axum::http::header::InvalidHeaderValue| {
                        ControlError::Adapter(error.to_string())
                    },
                )?,
            );
            let response = app
                .clone()
                .oneshot(request)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .to_bytes();
            let document: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                document["basePath"],
                format!("http://pbx.example:8088{base}")
            );
            assert_eq!(document["swaggerVersion"], version);
        }
        for host in ["", "person@pbx.example", "pbx.example/path"] {
            let mut request = request("GET", "/ari/api-docs/resources.json", Some("test"))?;
            request.headers_mut().insert(
                "Host",
                host.parse()
                    .map_err(|error: axum::http::header::InvalidHeaderValue| {
                        ControlError::Adapter(error.to_string())
                    })?,
            );
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .map_err(|error| ControlError::Adapter(error.to_string()))?
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}

#[tokio::test]
async fn authentication_origin_and_read_only_permissions_apply_to_all_resources(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/ari/channels", None)?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/ari/channels", Some("reader"))?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/ari/bridges?bridgeId=mix",
                Some("reader")
            )?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut wrong_origin = request("GET", "/ari/channels", Some("test"))?;
    wrong_origin.headers_mut().insert(
        "Origin",
        "https://untrusted.example".parse().map_err(
            |error: axum::http::header::InvalidHeaderValue| {
                ControlError::Adapter(error.to_string())
            },
        )?,
    );
    assert_eq!(
        app.oneshot(wrong_origin)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::FORBIDDEN
    );
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn http_bridge_lifecycle_and_known_unsupported_operations_have_ari_errors(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(config()?, 8);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/ari/bridges?bridgeId=mix&name=Conversation",
            Some("test"),
        )?)
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    assert_eq!(created.status(), StatusCode::OK);
    let bytes = created
        .into_body()
        .collect()
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .to_bytes();
    let bridge: rusterisk_api::Bridge = serde_json::from_slice(&bytes)?;
    assert_eq!(bridge.id.as_str(), "mix");
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", "/ari/bridges/mix", Some("test"))?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::NO_CONTENT
    );
    let missing = app
        .clone()
        .oneshot(request("GET", "/ari/bridges/mix", Some("test"))?)
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let bytes = missing
        .into_body()
        .collect()
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .to_bytes();
    let error: rusterisk_api::ApiError = serde_json::from_slice(&bytes)?;
    assert_eq!(error.message, "Bridge not found");
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/ari/channels/externalMedia",
                Some("test")
            )?)
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .status(),
        StatusCode::NOT_IMPLEMENTED
    );
    for path in ["/ari/channels/x/snoop", "/ari/channels/x/snoop/observer"] {
        assert_eq!(
            app.clone()
                .oneshot(request("POST", path, Some("test"))?)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .status(),
            StatusCode::NOT_IMPLEMENTED,
            "{path}"
        );
    }
    for path in [
        "/ari/unknown",
        "/ari/channels/x/unknown",
        "/ari/channels/x/play/extra/unexpected",
    ] {
        assert_eq!(
            app.clone()
                .oneshot(request("POST", path, Some("test"))?)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}
