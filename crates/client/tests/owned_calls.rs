use rusterisk_client::{api::ChannelState, CallMilestone, Client, Error, Originate};
use rusterisk_server::{Effect, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

#[tokio::test]
async fn owned_calls_wait_for_distinct_milestones_without_blocking_events_and_cancel_on_drop(
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
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let session = client.subscribe(&"app".to_owned().try_into()?).await?;
        let mut call = session
            .originate(Originate::new("PJSIP/phone".into())?)
            .await?;
        let accepted = call
            .wait(CallMilestone::Accepted, Duration::from_secs(2))
            .await?;
        assert_eq!(
            accepted.channel.as_ref().map(|channel| channel.state),
            Some(ChannelState::Down)
        );
        assert!(!accepted.progress.has_reached(CallMilestone::Stasis));
        let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
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
        call.wait(CallMilestone::Stasis, Duration::from_secs(2))
            .await?;
        call.wait(CallMilestone::Answered, Duration::from_secs(2))
            .await?;
        assert!(matches!(
            call.wait(CallMilestone::MediaConnected, Duration::from_millis(30))
                .await,
            Err(Error::Timeout)
        ));
        controller
            .signal(Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::MediaConnected,
            })
            .await
            .map_err(|_| Error::Closed)?;
        call.wait(CallMilestone::MediaConnected, Duration::from_secs(2))
            .await?;
        let id = call.id().clone();
        drop(call);
        let released = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|_| Error::Timeout)?
            .ok_or(Error::Closed)?;
        assert!(matches!(released, Effect::Release { key: released } if released == key));
        assert!(matches!(
            client.channel(&id).await,
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
