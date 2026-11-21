use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use futures_util::StreamExt;
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{
    http::router, Action, ControlError, Controller, Credentials, IncomingCall, IncomingTarget,
    Milestone, Signal,
};
use serde_json::Value;
use std::time::Duration;
use tower::ServiceExt;

fn fail(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
async fn open(app: &Router, path: &str) -> Result<axum::response::Response, ControlError> {
    let req = Request::builder()
        .uri(path)
        .header("Authorization", "Basic cmVhZGVyOnBhc3N3b3Jk")
        .body(Body::empty())
        .map_err(fail)?;
    Ok(app
        .clone()
        .oneshot(req)
        .await
        .unwrap_or_else(|never| match never {}))
}
async fn record(stream: &mut axum::body::BodyDataStream) -> Result<Value, ControlError> {
    let bytes = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .map_err(fail)?
        .ok_or(ControlError::Capacity)?
        .map_err(fail)?;
    assert!(bytes.ends_with(b"\n"));
    Ok(serde_json::from_slice(&bytes)?)
}
fn incoming(id: &str) -> Result<IncomingCall, ControlError> {
    Ok(IncomingCall {
        id: ChannelId::try_from(id.to_owned())?,
        endpoint: "desk".into(),
        caller: "123".into(),
        context: "home".into(),
        extension: "200".into(),
        target: IncomingTarget::Stasis {
            application: "phone".into(),
            arguments: Vec::new(),
        },
    })
}

#[tokio::test]
async fn observing_a_call_preserves_the_phone_app_and_stops_at_its_channel_lifetime(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=write\n[reader]\ntype=user\npassword=password\nread_only=yes\n",
        "",
        "",
        "",
    )
    .map_err(fail)?;
    let (controller, runner, _effects) = Controller::new(config, 6);
    let task = tokio::spawn(runner.run());
    let app = router(controller.clone());
    let result = async {
        let owner = Credentials::new("app".into(), "write".into())?;
        let mut phone = controller
            .subscribe(owner.clone(), None, "phone".into())
            .await?;
        let first = controller.incoming(incoming("selected")?).await?;
        let other = controller.incoming(incoming("other")?).await?;
        let path = "/rusterisk/observe/channels/selected";
        let response = open(&app, path).await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/x-ndjson");
        let mut stream = response.into_body().into_data_stream();
        let initial = record(&mut stream).await?;
        assert_eq!(initial["type"], "snapshot");
        assert_eq!(initial["channels"].as_array().map(Vec::len), Some(1));
        assert_eq!(initial["channels"][0]["id"], "selected");
        assert_eq!(initial["channels"][0]["generation"], first.generation);
        let interrupted = open(&app, path).await?;
        drop(interrupted);
        assert!(controller
            .execute(owner.clone(), None, Action::GetChannel(first.id.clone()))
            .await
            .is_ok());
        controller
            .signal(Signal::Milestone {
                key: other.clone(),
                milestone: Milestone::Answered,
            })
            .await?;
        controller
            .signal(Signal::Milestone {
                key: first.clone(),
                milestone: Milestone::Answered,
            })
            .await?;
        controller
            .execute(owner.clone(), None, Action::Hangup(first.id.clone()))
            .await?;
        assert_eq!(open(&app, path).await?.status(), StatusCode::NOT_FOUND);
        let replacement = controller.incoming(incoming("selected")?).await?;
        assert_ne!(first.generation, replacement.generation);
        let mut ended = false;
        while !ended {
            let event = record(&mut stream).await?;
            assert_eq!(event["type"], "event");
            assert_eq!(event["channel"]["id"], "selected");
            assert_eq!(event["channel"]["generation"], first.generation);
            assert!(event["timestamp"]
                .as_str()
                .is_some_and(|s| s.parse::<jiff::Timestamp>().is_ok()));
            ended = event["event"] == "ChannelDestroyed";
        }
        assert!(stream.next().await.is_none());
        assert!(controller
            .execute(owner, None, Action::GetChannel(other.id))
            .await
            .is_ok());
        assert!(phone.events.try_recv().is_ok());
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(fail)??;
    result
}

#[tokio::test]
async fn an_endpoint_reader_detects_lag_and_releases_its_slot_on_disconnect(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=write\n[reader]\ntype=user\npassword=password\nread_only=yes\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=local\n[a]\ntype=aor\ncontact=sip:p@127.0.0.1:5061\n[desk]\ntype=endpoint\ntransport=udp\nauth=auth\naors=a\nallow=ulaw\n",
        "", "").map_err(fail)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let app = router(controller.clone());
        let path = "/rusterisk/observe/endpoints/PJSIP/desk";
        let anonymous = Request::builder()
            .uri(path)
            .body(Body::empty())
            .map_err(fail)?;
        assert_eq!(
            app.clone()
                .oneshot(anonymous)
                .await
                .unwrap_or_else(|never| match never {})
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let owner = Credentials::new("app".into(), "write".into())?;
        let mut phone = controller
            .subscribe(owner.clone(), None, "phone".into())
            .await?;
        let mut stream = open(&app, path).await?.into_body().into_data_stream();
        let initial = record(&mut stream).await?;
        assert_eq!(initial["endpoint"]["resource"], "desk");
        assert_eq!(initial["channels"].as_array().map(Vec::len), Some(0));
        let call = controller.incoming(incoming("owned")?).await?;
        let created = record(&mut stream).await?;
        assert_eq!(created["channel"]["id"], "owned");
        assert_eq!(created["channel"]["generation"], call.generation);
        for index in 0..150 {
            controller
                .execute(
                    owner.clone(),
                    None,
                    Action::SetVariable {
                        channel: call.id.clone(),
                        variable: "PRIVATE".into(),
                        value: Some(index.to_string()),
                    },
                )
                .await?;
            while phone.events.try_recv().is_ok() {}
        }
        let gap = record(&mut stream).await?;
        assert_eq!(gap["type"], "gap");
        assert_eq!(gap["reason"], "reader_lagged");
        assert!(gap["skipped"].as_u64().is_some_and(|n| n > 0));
        assert!(stream.next().await.is_none());
        let mut watches = Vec::new();
        for _ in 0..64 {
            let response = open(&app, path).await?;
            assert_eq!(response.status(), StatusCode::OK);
            watches.push(response);
        }
        assert_eq!(
            open(&app, path).await?.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(watches);
        assert_eq!(open(&app, path).await?.status(), StatusCode::OK);
        assert!(controller
            .execute(owner, None, Action::GetChannel(call.id))
            .await
            .is_ok());
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await.map_err(fail)??;
    result
}
