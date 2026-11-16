use axum::{body::Body, http::Request, Router};
use http_body_util::BodyExt;
use rusterisk_config::Config;
use rusterisk_server::{http::router, ControlError, Controller, Credentials, Effect};
use serde_json::Value;
use std::time::Duration;
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: &'static str,
) -> Result<(u16, Value), ControlError> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Basic YXBwOnRlc3QtcGFzc3dvcmQ=")
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .map_err(failure)?;
    let response = app.clone().oneshot(request).await.map_err(failure)?;
    let status = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(failure)?
        .to_bytes();
    Ok((status, serde_json::from_slice(&bytes)?))
}

#[tokio::test]
async fn originate_with_id_preserves_the_path_and_validated_body_fields() -> Result<(), ControlError>
{
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
    let operation = tokio::time::timeout(Duration::from_secs(3), async {
        let _subscription = controller.subscribe(
            Credentials::new("app".into(), "test-password".into())?,
            None,
            "calls".into(),
        ).await?;
        let app = router(controller.clone());
        let path = "/ari/channels/path-call?endpoint=PJSIP/query&app=query&channelId=query-call";
        let body = r#"{"endpoint":"PJSIP/phone","app":"calls","channelId":"body-call","callerId":"101","variables":{"MARK":"from body"}}"#;
        let (status, channel) = request(&app, "POST", path, body).await?;
        if status != 200 {
            return Err(failure(format!("originateWithId returned {status}: {channel}")));
        }
        if channel["id"] != "path-call" || channel["caller"]["number"] != "101" {
            return Err(failure("originateWithId changed its path or caller"));
        }
        let effect = effects.recv().await.ok_or_else(|| failure("missing originate effect"))?;
        if !matches!(effect, Effect::Start { key, endpoint, caller, .. } if key.id.as_str() == "path-call" && endpoint == "PJSIP/phone" && caller == "101") {
            return Err(failure("originateWithId changed its signaling effect"));
        }
        let (status, variable) = request(&app, "GET", "/ari/channels/path-call/variable?variable=MARK", "").await?;
        if status != 200 || variable["value"] != "from body" {
            return Err(failure("originateWithId lost its initial variables"));
        }
        for other in ["query-call", "body-call"] {
            if request(&app, "GET", &format!("/ari/channels/{other}"), "").await?.0 != 404 {
                return Err(failure("originateWithId admitted an overridden identifier"));
            }
        }
        if request(&app, "POST", path, body).await?.0 != 409 {
            return Err(failure("duplicate originateWithId was not rejected"));
        }
        if request(&app, "POST", "/ari/channels/create?endpoint=PJSIP/phone&app=calls", "").await?.0 != 501 {
            return Err(failure("originateWithId captured the create operation"));
        }
        Ok(())
    }).await.map_err(failure).and_then(|result| result);
    controller.shutdown();
    let cleanup = task.await.map_err(failure).and_then(|result| result);
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(failure(format!(
            "{operation}; control cleanup failed: {cleanup}"
        ))),
    }
}
