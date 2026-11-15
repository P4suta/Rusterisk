use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use rusterisk_api::Bridge;
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn registered_json_fields_override_query_values_without_overriding_path_identifiers(
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
    let runner = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        for (path, body, expected_id, expected_name) in [
            ("/ari/bridges?bridgeId=query&name=query", r#"{"bridgeId":"body","name":"Body name","type":"mixing","unknown":{"ignored":true}}"#, "body", "Body name"),
            ("/ari/bridges/path?name=query", r#"{"bridgeId":"intruder","name":"Path name","type":"mixing"}"#, "path", "Path name"),
            ("/ari/bridges/cleared?name=query", r#"{"name":false}"#, "cleared", ""),
        ] {
            let request = Request::builder().method("POST").uri(path)
                .header("Authorization", "Basic YXBwOnRlc3QtcGFzc3dvcmQ=")
                .header("Content-Type", "application/json")
                .body(Body::from(body)).map_err(failure)?;
            let response = app.clone().oneshot(request).await.map_err(failure)?;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let bridge: Bridge = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
            assert_eq!(bridge.id.as_str(), expected_id);
            assert_eq!(bridge.name, expected_name);
        }
        Ok::<(), ControlError>(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn json_credentials_cannot_authorize_requests_and_unavailable_adapters_stay_identifiable(
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
    let runner = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        for (path, authenticated, body, status) in [
            (
                "/ari/bridges",
                false,
                r#"{"api_key":"app:test-password","bridgeId":"body"}"#,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/ari/channels/missing/snoop",
                true,
                r#"{"app":"calls"}"#,
                StatusCode::NOT_IMPLEMENTED,
            ),
            (
                "/ari/unknown",
                true,
                r#"{"name":"ignored"}"#,
                StatusCode::NOT_FOUND,
            ),
            ("/ari/bridges", true, "[]", StatusCode::BAD_REQUEST),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", "application/json");
            if authenticated {
                request = request.header("Authorization", "Basic YXBwOnRlc3QtcGFzc3dvcmQ=");
            }
            let request = request.body(Body::from(body)).map_err(failure)?;
            assert_eq!(
                app.clone()
                    .oneshot(request)
                    .await
                    .map_err(failure)?
                    .status(),
                status
            );
        }
        Ok::<(), ControlError>(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    result
}
