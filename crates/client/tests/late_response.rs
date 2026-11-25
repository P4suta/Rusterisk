use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use rusterisk_client::{CallCompletion, CallMilestone, Client, Error, Originate};
use rusterisk_server::test_time::Wait;
use rusterisk_server::{Effect, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};
use tokio::sync::{mpsc, oneshot};

async fn delay_originate(
    State(pending): State<mpsc::Sender<oneshot::Sender<()>>>,
    request: Request,
    next: Next,
) -> Response {
    let delay = request.method() == "POST" && request.uri().path() == "/ari/channels";
    let response = next.run(request).await;
    if delay {
        let (release, released) = oneshot::channel();
        if pending.send(release).await.is_ok() {
            let _released = released.await;
        }
    }
    response
}

#[tokio::test]
async fn cancellation_ends_an_observed_channel_before_its_delayed_originate_response(
) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| Error::Invalid("test configuration"))?;
    let (controller, runner, mut effects) = rusterisk_server::Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (pending, mut delayed) = mpsc::channel(4);
    let app = rusterisk_server::http::router(controller.clone()).layer(
        axum::middleware::from_fn_with_state(pending, delay_originate),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let session = client.subscribe(&"app".to_owned().try_into()?).await?;
        let mut call = session
            .originate(Originate::new("PJSIP/phone".into())?)
            .await?;
        let release = Wait::Ready
            .bounded(delayed.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        let effect = effects.recv().await.ok_or(Error::Closed)?;
        let Effect::Start { key, .. } = effect else {
            return Err(Error::Invalid("expected start effect"));
        };
        controller
            .signal(Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            })
            .await
            .map_err(|_| Error::Closed)?;
        let entered = call
            .wait(CallMilestone::Stasis, Duration::from_secs(2))
            .await?;
        assert!(entered.progress.has_reached(CallMilestone::Answered));
        assert!(!entered.progress.has_reached(CallMilestone::Accepted));
        call.cancel();
        assert_eq!(
            call.snapshot().progress.completion(),
            Some(CallCompletion::Cancelled)
        );
        assert!(matches!(
            call.wait(CallMilestone::Accepted, Duration::from_secs(2))
                .await,
            Err(Error::CallEnded(CallCompletion::Cancelled))
        ));
        let effect = Wait::Ready
            .bounded(effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        assert!(matches!(effect, Effect::Release { key: ended } if ended == key));
        release.send(()).map_err(|_| Error::Closed)?;
        call.close().await?;
        assert!(matches!(
            client.channel(&key.id).await,
            Err(Error::Api { status: 404, .. })
        ));
        session.shutdown().await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    server.abort();
    assert!(server.await.is_err());
    result
}
