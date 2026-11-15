use rusterisk_client::{
    api::{ChannelId, ChannelState, EventKind},
    CallMilestone, Client, Error,
};
use rusterisk_server::{Controller, IncomingCall, IncomingTarget, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

#[tokio::test]
async fn incoming_adoption_preserves_early_events_and_owns_terminal_cleanup() -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[api]\ntype=user\npassword=password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let client = Client::new(&format!("http://{address}"),"api","password")?;
        let mut session = client.subscribe(&ChannelId::try_from("calls".to_owned())?).await?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        assert!(matches!(session.adopt(&id).await, Err(Error::Invalid(_))));
        let key = controller.incoming(IncomingCall { id:id.clone(), endpoint:"phone".into(), caller:"100".into(), context:"internal".into(), extension:"200".into(), target:IncomingTarget::Stasis { application:"calls".into(), arguments:Vec::new() } }).await.map_err(failure)?;
        controller.signal(Signal::Milestone { key:key.clone(), milestone:Milestone::Answered }).await.map_err(failure)?;
        loop {
            if matches!(session.next_event().await?.kind, EventKind::ChannelStateChange { channel } if channel.id == id && channel.state == ChannelState::Up) { break; }
        }
        let mut call = session.adopt(&id).await?;
        let snapshot = call.snapshot();
        assert!(snapshot.progress.has_reached(CallMilestone::Stasis));
        assert!(snapshot.progress.has_reached(CallMilestone::Answered));
        assert!(!snapshot.progress.has_reached(CallMilestone::Accepted));
        assert!(!snapshot.progress.has_reached(CallMilestone::MediaConnected));
        assert!(matches!(session.adopt(&id).await, Err(Error::Invalid(_))));
        controller.signal(Signal::Milestone { key, milestone:Milestone::MediaConnected }).await.map_err(failure)?;
        call.wait(CallMilestone::MediaConnected, Duration::from_secs(1)).await?;
        call.close().await?;
        assert!(matches!(client.channel(&id).await, Err(Error::Api { status:404, .. })));
        session.shutdown().await?;
        Ok::<(), Error>(())
    }).await.map_err(|_| Error::Timeout).and_then(|result| result);
    controller.shutdown();
    runner.await.map_err(failure)?.map_err(failure)?;
    server.abort();
    assert!(server.await.is_err_and(|error| error.is_cancelled()));
    result
}
