use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use rusterisk_client::{CallCompletion, CallMilestone, Client, Error, Originate};
use std::{future::IntoFuture, time::Duration};

async fn refuse_hangup(request: Request, next: Next) -> Response {
    if request.method() == "DELETE" && request.uri().path().starts_with("/ari/channels/") {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(rusterisk_client::api::ApiError {
                message: "simulated cleanup failure".into(),
            }),
        )
            .into_response();
    }
    next.run(request).await
}

struct Harness {
    client: Client,
    controller: rusterisk_server::Controller,
    runner: tokio::task::JoinHandle<Result<(), rusterisk_server::ControlError>>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _effects: tokio::sync::mpsc::Receiver<rusterisk_server::Effect>,
}

impl Harness {
    async fn start(failure: Failure) -> Result<Self, Error> {
        let config = rusterisk_config::Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "",
            "",
            "",
        )
        .map_err(|_| Error::Invalid("test configuration"))?;
        let (controller, runner, effects) = rusterisk_server::Controller::new(config, 4);
        let runner = tokio::spawn(runner.run());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = rusterisk_server::http::router(controller.clone());
        let app = match failure {
            Failure::None => app,
            Failure::Hangup => app.layer(axum::middleware::from_fn(refuse_hangup)),
        };
        let server = tokio::spawn(axum::serve(listener, app).into_future());
        Ok(Self {
            client: Client::new(&format!("http://{address}"), "app", "test-password")?,
            controller,
            runner,
            server,
            _effects: effects,
        })
    }
    async fn shutdown(self) -> Result<(), Error> {
        self.controller.shutdown();
        self.runner
            .await
            .map_err(|_| Error::Closed)?
            .map_err(|_| Error::Closed)?;
        self.server.abort();
        assert!(self.server.await.is_err());
        Ok(())
    }
}

enum Failure {
    None,
    Hangup,
}

#[tokio::test]
async fn callers_can_wait_for_peer_completion_without_reading_the_event_queue() -> Result<(), Error>
{
    let harness = Harness::start(Failure::None).await?;
    let result = async {
        let session = harness
            .client
            .subscribe(&"app".to_owned().try_into()?)
            .await?;
        let mut call = session
            .originate(Originate::new("PJSIP/phone".into())?)
            .await?;
        call.wait(CallMilestone::Accepted, Duration::from_secs(2))
            .await?;
        assert!(matches!(
            call.wait_completion(Duration::from_millis(20)).await,
            Err(Error::Timeout)
        ));
        harness.client.hangup(call.id()).await?;
        let completed = call.wait_completion(Duration::from_secs(2)).await?;
        assert_eq!(completed.progress.completion(), Some(CallCompletion::Ended));
        call.close().await?;
        session.shutdown().await?;
        Ok(())
    }
    .await;
    harness.shutdown().await?;
    result
}

#[tokio::test]
async fn dropping_a_session_cleans_calls_while_their_handles_are_still_alive() -> Result<(), Error>
{
    let harness = Harness::start(Failure::None).await?;
    let result = async {
        let session = harness
            .client
            .subscribe(&"app".to_owned().try_into()?)
            .await?;
        let mut calls = Vec::new();
        for _ in 0..2 {
            let mut call = session
                .originate(Originate::new("PJSIP/phone".into())?)
                .await?;
            call.wait(CallMilestone::Accepted, Duration::from_secs(2))
                .await?;
            calls.push(call);
        }
        drop(session);
        for call in calls {
            call.close().await?;
        }
        assert!(harness.client.channels().await?.is_empty());
        Ok(())
    }
    .await;
    harness.shutdown().await?;
    result
}

#[tokio::test]
async fn failed_admission_preserves_the_api_error_and_cleanup_failures_are_observable(
) -> Result<(), Error> {
    let harness = Harness::start(Failure::Hangup).await?;
    let result = async {
        let session = harness.client.subscribe(&"app".to_owned().try_into()?).await?;
        let mut refused = session.originate(Originate::new("Local/unsupported".into())?).await?;
        assert!(matches!(refused.wait(CallMilestone::Accepted, Duration::from_secs(2)).await, Err(Error::CallFailed(failure)) if failure.status == Some(400)));
        assert_eq!(refused.snapshot().progress.completion(), Some(CallCompletion::Failed));
        refused.close().await?;
        let mut active = session.originate(Originate::new("PJSIP/phone".into())?).await?;
        active.wait(CallMilestone::Accepted, Duration::from_secs(2)).await?;
        assert!(matches!(active.close().await, Err(Error::Cleanup(message)) if message.contains("simulated cleanup failure")));
        assert!(matches!(session.shutdown().await, Err(Error::Cleanup(message)) if message.contains("simulated cleanup failure")));
        Ok(())
    }.await;
    harness.shutdown().await?;
    result
}
