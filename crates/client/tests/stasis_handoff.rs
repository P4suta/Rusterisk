use rusterisk_client::{api, CallCompletion, CallMilestone, Client, Error, Originate};
use rusterisk_server::{Action, Controller, Credentials, Effect, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

enum Handoff {
    App,
    Dialplan,
}

async fn handoff(operation: Handoff) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[api]\ntype=user\npassword=password\n",
        "",
        "",
        "[destination]\nexten => s,1,Wait(60)\nsame => n,Hangup()\n",
    )
    .map_err(|_| Error::Invalid("test configuration"))?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let client = Client::new(&format!("http://{address}"), "api", "password")?;
        let source = client.subscribe(&api::ChannelId::try_from("source".to_owned())?).await?;
        let mut destination = client.subscribe(&api::ChannelId::try_from("destination".to_owned())?).await?;
        let mut call = source.originate(Originate::new("PJSIP/phone".into())?).await?;
        call.wait(CallMilestone::Accepted, Duration::from_secs(2)).await?;
        let Effect::Start { key, .. } = tokio::time::timeout(Duration::from_secs(2), effects.recv()).await.map_err(|_| Error::Timeout)?.ok_or(Error::Closed)? else { return Err(Error::Invalid("expected start effect")); };
        controller.signal(Signal::Milestone { key, milestone: Milestone::Answered }).await.map_err(|_| Error::Closed)?;
        call.wait(CallMilestone::Stasis, Duration::from_secs(2)).await?;
        let id = call.id().clone();
        let action = match operation {
            Handoff::App => Action::Move { channel: id.clone(), app: "destination".into(), args: Vec::new() },
            Handoff::Dialplan => Action::Continue { channel: id.clone(), context: Some("destination".into()), extension: None, priority: None },
        };
        controller.execute(Credentials::new("api".into(), "password".into()).map_err(|_| Error::Closed)?, None, action).await.map_err(|_| Error::Closed)?;
        let released = call.wait_completion(Duration::from_secs(2)).await?;
        assert_eq!(released.progress.completion(), Some(CallCompletion::StasisExited));
        assert_eq!(released.cleanup, rusterisk_client::Cleanup::Complete);
        assert!(matches!(call.wait(CallMilestone::MediaConnected, Duration::from_secs(1)).await, Err(Error::StasisExited)));
        assert_eq!(client.channel(&id).await?.state, api::ChannelState::Up);
        if matches!(operation, Handoff::App) {
            let entered = tokio::time::timeout(Duration::from_secs(2), destination.next_event()).await.map_err(|_| Error::Timeout)??;
            assert!(matches!(entered.kind, api::EventKind::StasisStart { channel, .. } if channel.id == id));
        }
        call.close().await?;
        source.shutdown().await?;
        assert_eq!(client.channel(&id).await?.state, api::ChannelState::Up);
        client.hangup(&id).await?;
        destination.shutdown().await?;
        Ok(())
    }.await;
    controller.shutdown();
    runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed)?;
    server.abort();
    assert!(server.await.is_err());
    result
}

#[tokio::test]
async fn leaving_stasis_preserves_the_destination_call_when_the_handle_and_session_close(
) -> Result<(), Error> {
    handoff(Handoff::App).await?;
    handoff(Handoff::Dialplan).await
}
