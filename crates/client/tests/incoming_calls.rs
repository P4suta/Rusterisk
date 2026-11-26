use rusterisk_client::{
    api::{ChannelId, ChannelState, EventKind},
    CallCompletion, CallMilestone, Cleanup, Client, Error,
};
use rusterisk_server::test_time::Wait;
use rusterisk_server::{Controller, IncomingCall, IncomingTarget, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

#[tokio::test]
async fn incoming_adoption_preserves_early_events_and_owns_terminal_cleanup() -> Result<(), Error> {
    incoming(End::Handle).await
}

#[tokio::test]
async fn session_shutdown_cleans_adopted_incoming_calls_with_live_handles() -> Result<(), Error> {
    incoming(End::Session).await
}

#[tokio::test]
async fn incoming_handoff_releases_the_handle_without_ending_the_destination_call(
) -> Result<(), Error> {
    incoming(End::Handoff).await
}

enum End {
    Handle,
    Session,
    Handoff,
}

async fn incoming(end: End) -> Result<(), Error> {
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
    let result = Wait::Ready.bounded(async {
        let client = Client::new(&format!("http://{address}"),"api","password")?;
        let mut session = client.subscribe(&ChannelId::try_from("calls".to_owned())?).await?;
        let mut destination = client.subscribe(&ChannelId::try_from("destination".to_owned())?).await?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        assert!(matches!(session.adopt(&id).await, Err(Error::Invalid(_))));
        let key = controller.incoming(IncomingCall { id:id.clone(), endpoint:"phone".into(), caller:"100".into(), context:"internal".into(), extension:"200".into(), target:IncomingTarget::Stasis { application:"calls".into(), arguments:Vec::new() } }).await.map_err(failure)?;
        if !matches!(end, End::Handle) {
            controller.signal(Signal::Milestone { key:key.clone(), milestone:Milestone::MediaConnected }).await.map_err(failure)?;
        }
        controller.signal(Signal::Milestone { key:key.clone(), milestone:Milestone::Answered }).await.map_err(failure)?;
        loop {
            if matches!(session.next_event().await?.kind, EventKind::ChannelStateChange { channel } if channel.id == id && channel.state == ChannelState::Up) { break; }
        }
        let mut call = session.adopt(&id).await?;
        let snapshot = call.snapshot();
        assert!(snapshot.progress.has_reached(CallMilestone::Stasis));
        assert!(snapshot.progress.has_reached(CallMilestone::Answered));
        assert!(!snapshot.progress.has_reached(CallMilestone::Accepted));
        assert_eq!(snapshot.progress.has_reached(CallMilestone::MediaConnected), !matches!(end, End::Handle));
        assert!(matches!(session.adopt(&id).await, Err(Error::Invalid(_))));
        if matches!(end, End::Handle) {
            controller.signal(Signal::Milestone { key, milestone:Milestone::MediaConnected }).await.map_err(failure)?;
        }
        call.wait(CallMilestone::MediaConnected, Duration::from_secs(1)).await?;
        match end {
            End::Handle => {
                call.close().await?;
                session.shutdown().await?;
                assert!(matches!(client.channel(&id).await, Err(Error::Api { status:404, .. })));
            }
            End::Session => {
                session.shutdown().await?;
                let snapshot = call.wait_completion(Duration::from_secs(1)).await?;
                assert_eq!(snapshot.progress.completion(), Some(CallCompletion::Cancelled));
                assert_eq!(snapshot.cleanup, Cleanup::Complete);
                call.close().await?;
                assert!(matches!(client.channel(&id).await, Err(Error::Api { status:404, .. })));
            }
            End::Handoff => {
                let app = ChannelId::try_from("destination".to_owned())?;
                let comma = ["ambiguous,argument".into()];
                let empty = [String::new()];
                let long = ["x".repeat(257)];
                let overflow: [String;33] = std::array::from_fn(|_| "x".into());
                let control = ["x\r\n".into()];
                for invalid in [comma.as_slice(), empty.as_slice(), long.as_slice(), overflow.as_slice(), control.as_slice()] {
                    assert!(matches!(client.move_channel(&id, &app, invalid).await, Err(Error::Invalid(_))));
                }
                assert!(matches!(client.move_channel(&id, &app, &vec!["x".repeat(256);32]).await, Err(Error::Capacity)));
                let arguments = vec!["x".repeat(127);32];
                client.move_channel(&id, &app, &arguments).await?;
                let snapshot = call.wait_completion(Duration::from_secs(1)).await?;
                assert_eq!(snapshot.progress.completion(), Some(CallCompletion::StasisExited));
                assert_eq!(snapshot.cleanup, Cleanup::Complete);
                assert!(matches!(session.adopt(&id).await, Err(Error::Invalid(_))));
                let event = destination.next_event().await?;
                assert!(matches!(event.kind, EventKind::StasisStart { channel, args } if channel.id == id && args == arguments));
                call.close().await?;
                session.shutdown().await?;
                assert_eq!(client.channel(&id).await?.state, ChannelState::Up);
                let adopted = destination.adopt(&id).await?;
                adopted.close().await?;
                assert!(matches!(client.channel(&id).await, Err(Error::Api { status:404, .. })));
            }
        }
        destination.shutdown().await?;
        Ok::<(), Error>(())
    }).await.map_err(|_| Error::Timeout).and_then(|result| result);
    controller.shutdown();
    runner.await.map_err(failure)?.map_err(failure)?;
    server.abort();
    assert!(server.await.is_err_and(|error| error.is_cancelled()));
    result
}
