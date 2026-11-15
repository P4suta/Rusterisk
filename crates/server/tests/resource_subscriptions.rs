use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use http_body_util::BodyExt;
use rusterisk_api::{Application, ChannelId, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, Action, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
    Milestone, Signal,
};
use tower::ServiceExt;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn request(method: &str, source: &str) -> Result<Request<Body>, ControlError> {
    Request::builder()
        .method(method)
        .uri(format!(
            "/ari/applications/observer/subscription?eventSource={source}"
        ))
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("app:test-password")
            ),
        )
        .body(Body::empty())
        .map_err(failure)
}

fn incoming(id: &ChannelId) -> IncomingCall {
    IncomingCall {
        id: id.clone(),
        endpoint: "phone".into(),
        caller: "100".into(),
        context: "internal".into(),
        extension: "100".into(),
        target: IncomingTarget::Stasis {
            application: "calls".into(),
            arguments: Vec::new(),
        },
    }
}

#[tokio::test]
async fn endpoint_observers_receive_availability_and_specific_endpoint_channels(
) -> Result<(), ControlError> {
    let configuration = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[dynamic]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(configuration, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _calls = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let mut observer = controller.subscribe(credentials.clone(), None, "observer".into()).await?;
        let mut all = controller.subscribe(credentials.clone(), None, "all-endpoints".into()).await?;
        controller.execute(credentials.clone(), None, Action::SubscribeResources { application: "all-endpoints".into(), sources: ["endpoint:".to_owned()].into_iter().collect() }).await?;
        let app = router(controller.clone());
        let response = app.clone().oneshot(request("POST", "endpoint:PJSIP/phone")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot: Application = serde_json::from_slice(&response.into_body().collect().await.map_err(failure)?.to_bytes())?;
        assert_eq!(snapshot.endpoint_ids, ["PJSIP/phone"]);
        controller.signal(Signal::EndpointContact { key: rusterisk_server::EndpointKey { resource: "phone".into(), generation: 1 }, presence: rusterisk_core::ContactPresence::Present }).await?;
        for subscription in [&mut observer, &mut all] {
            let event = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
            assert!(matches!(event.kind, EventKind::EndpointStateChange { endpoint } if endpoint.state == Some(rusterisk_api::EndpointState::Online)));
        }
        let id = ChannelId::try_from("endpoint-call".to_owned())?;
        controller.incoming(incoming(&id)).await?;
        let created = observer.events.try_recv().map_err(failure)?;
        assert!(matches!(created.kind, EventKind::ChannelCreated { channel } if channel.id == id));
        assert!(observer.events.try_recv().is_err());
        assert!(all.events.try_recv().is_err());
        let response = app.oneshot(request("POST", "endpoint:pjsip/phone")?).await.map_err(failure)?;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        Ok(())
    }.await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn channel_observers_count_interests_and_end_before_identifier_reuse(
) -> Result<(), ControlError> {
    let configuration = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(configuration, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut calls = controller
            .subscribe(credentials.clone(), None, "calls".into())
            .await?;
        let mut observer = controller
            .subscribe(credentials.clone(), None, "observer".into())
            .await?;
        let id = ChannelId::try_from("call".to_owned())?;
        let key = controller.incoming(incoming(&id)).await?;
        let app = router(controller.clone());
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(request("POST", "channel:call")?)
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), StatusCode::OK);
            let snapshot: Application = serde_json::from_slice(
                &response
                    .into_body()
                    .collect()
                    .await
                    .map_err(failure)?
                    .to_bytes(),
            )?;
            assert_eq!(snapshot.channel_ids, std::slice::from_ref(&id));
        }
        let response = app
            .clone()
            .oneshot(request("DELETE", "channel:call")?)
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), StatusCode::OK);
        controller
            .signal(Signal::Milestone {
                key,
                milestone: Milestone::Answered,
            })
            .await?;
        let answered =
            tokio::time::timeout(std::time::Duration::from_secs(1), observer.events.recv())
                .await
                .map_err(failure)?
                .ok_or(ControlError::Capacity)?;
        assert!(matches!(
            answered.kind,
            EventKind::ChannelStateChange { .. }
        ));
        controller
            .execute(credentials.clone(), None, Action::Hangup(id.clone()))
            .await?;
        let terminal = observer.events.try_recv().map_err(failure)?;
        assert!(matches!(terminal.kind, EventKind::ChannelDestroyed { .. }));
        assert!(observer.events.try_recv().is_err());
        while calls.events.try_recv().is_ok() {}
        let key = controller.incoming(incoming(&id)).await?;
        controller
            .signal(Signal::Milestone {
                key,
                milestone: Milestone::Answered,
            })
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for _ in 0..8 {
                let event = calls.events.recv().await.ok_or(ControlError::Capacity)?;
                if matches!(event.kind, EventKind::ChannelStateChange { .. }) {
                    return Ok(());
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)??;
        assert!(observer.events.try_recv().is_err());
        let response = app
            .clone()
            .oneshot(request("DELETE", "channel:call")?)
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        drop(observer);
        assert_eq!(
            controller
                .execute(credentials, None, Action::ListChannels)
                .await?
                .body
                .and_then(|body| body.as_array().map(Vec::len)),
            Some(1)
        );
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result
}
