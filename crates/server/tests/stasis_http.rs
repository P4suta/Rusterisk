use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use rusterisk_api::{ChannelId, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
};
use rusterisk_test_support::Wait;
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
async fn http_continuation_validates_positions_before_leaving_and_resumes_without_blocking_events(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=password\n", "", "", "[internal]\nexten => 200,1,Stasis(app,first)\nsame => n,Stasis(app,second)\n[other]\nexten => s,1,Stasis(app,moved)\n").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let mut subscription = controller
            .subscribe(
                Credentials::new("app".into(), "password".into())?,
                None,
                "app".into(),
            )
            .await?;
        controller
            .incoming(IncomingCall {
                id: ChannelId::try_from("incoming".to_owned())?,
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "200".into(),
                target: IncomingTarget::Dialplan,
            })
            .await?;
        subscription
            .events
            .recv()
            .await
            .ok_or(ControlError::Capacity)?;
        subscription
            .events
            .recv()
            .await
            .ok_or(ControlError::Capacity)?;
        let app = router(controller.clone());
        for priority in ["-1", "text", "65536"] {
            let response = app
                .clone()
                .oneshot(request(&format!(
                    "/ari/channels/incoming/continue?priority={priority}"
                ))?)
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(subscription.events.try_recv().is_err());
        }
        for (query, expected) in [("", "second"), ("?context=other", "moved")] {
            let response = app
                .clone()
                .oneshot(request(&format!("/ari/channels/incoming/continue{query}"))?)
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let ended = Wait::Ready
                .bounded(subscription.events.recv())
                .await
                .map_err(failure)?
                .ok_or(ControlError::Capacity)?;
            assert!(matches!(ended.kind, EventKind::StasisEnd { .. }));
            Wait::Ready
                .bounded(async {
                    while let Some(event) = subscription.events.recv().await {
                        if let EventKind::StasisStart { args, .. } = event.kind {
                            assert_eq!(args, [expected]);
                            return Ok(());
                        }
                    }
                    Err(ControlError::Capacity)
                })
                .await
                .map_err(failure)??;
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn http_move_routes_events_and_cleanup_to_the_current_stasis_app() -> Result<(), ControlError>
{
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "[internal]\nexten => 200,1,Stasis(app)\n",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "password".into())?;
        let mut previous = controller.subscribe(credentials.clone(), None, "app".into()).await?;
        let mut destination = controller.subscribe(credentials.clone(), None, "destination".into()).await?;
        let key = controller.incoming(IncomingCall { id: ChannelId::try_from("incoming".to_owned())?, endpoint: "phone".into(), caller: "100".into(), context: "internal".into(), extension: "200".into(), target: IncomingTarget::Dialplan }).await?;
        previous.events.recv().await.ok_or(ControlError::Capacity)?;
        previous.events.recv().await.ok_or(ControlError::Capacity)?;
        let app = router(controller.clone());
        let failed = app.clone().oneshot(request("/ari/channels/incoming/move?app=missing&appArgs=retry")?).await.map_err(failure)?;
        assert_eq!(failed.status(), StatusCode::NO_CONTENT);
        let failed = previous.events.recv().await.ok_or(ControlError::Capacity)?;
        assert!(matches!(failed.kind, EventKind::ApplicationMoveFailed { args, .. } if args == ["retry"]));
        let moved = app.oneshot(request("/ari/channels/incoming/move?app=destination&appArgs=arrived,second")?).await.map_err(failure)?;
        assert_eq!(moved.status(), StatusCode::NO_CONTENT);
        assert!(matches!(previous.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::StasisEnd { .. }));
        assert!(matches!(destination.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::StasisStart { args, .. } if args == ["arrived", "second"]));
        drop(previous);
        controller.execute(credentials.clone(), None, rusterisk_server::Action::GetChannel(key.id.clone())).await?;
        assert!(effects.try_recv().is_err());
        drop(destination);
        let released = Wait::Ready.bounded(effects.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
        assert!(matches!(released, rusterisk_server::Effect::Release { key: ended } if ended == key));
        Ok(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
