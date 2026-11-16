use axum::{body::Body, http::Request};
use base64::Engine;
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, ControlError, Controller, Credentials, IncomingCall, IncomingTarget, Milestone,
    Signal,
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
async fn playback_timing_validates_before_admission_and_preserves_both_target_options(
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
    let operation = tokio::time::timeout(Duration::from_secs(3), async {
        let _subscription = controller
            .subscribe(
                Credentials::new("app".into(), "password".into())?,
                None,
                "calls".into(),
            )
            .await?;
        let key = controller
            .incoming(IncomingCall {
                id: ChannelId::try_from("phone".to_owned())?,
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "calls".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        controller
            .signal(Signal::Milestone {
                key,
                milestone: Milestone::Answered,
            })
            .await?;
        let app = router(controller.clone());
        for field in ["offsetms", "skipms"] {
            for value in ["-1", "2147483648", "4294967296", "1.5", "invalid"] {
                let response = app
                    .clone()
                    .oneshot(request(&format!(
                        "/ari/channels/phone/play/rejected?media=sound:welcome&{field}={value}"
                    ))?)
                    .await
                    .map_err(failure)?;
                if response.status() != axum::http::StatusCode::BAD_REQUEST {
                    return Err(failure(format!(
                        "{field}={value} returned {}",
                        response.status()
                    )));
                }
            }
        }
        if effects.try_recv().is_ok() {
            return Err(failure("invalid playback timing emitted a media effect"));
        }
        let created = app.clone().oneshot(request("/ari/bridges?bridgeId=timed-mix")?).await.map_err(failure)?;
        if created.status() != axum::http::StatusCode::OK {
            return Err(failure("could not prepare playback bridge"));
        }
        for path in ["/ari/channels/phone/play/timed-channel", "/ari/bridges/timed-mix/play/timed-bridge"] {
            let response = app.clone().oneshot(request(&format!("{path}?media=sound:welcome&offsetms=1234&skipms=5678"))?).await.map_err(failure)?;
            if response.status() != axum::http::StatusCode::CREATED {
                return Err(failure(format!("valid playback timing returned {}", response.status())));
            }
            if !matches!(effects.recv().await, Some(rusterisk_server::Effect::PlayMedia { options, .. }) if options.offset_ms.milliseconds() == 1234 && options.step_ms() == 5678) {
                return Err(failure("playback timing was not preserved in the media effect"));
            }
        }
        Ok(())
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
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
