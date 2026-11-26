use rusterisk_client::{api::ChannelId, Client, Error};
use rusterisk_config::Config;
use rusterisk_server::test_time::Wait;
use rusterisk_server::{
    Action, Controller, Credentials, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use std::future::IntoFuture;

#[tokio::test]
async fn stopping_during_peer_setup_releases_both_channels_and_the_bridge() -> Result<(), Error> {
    setup_cleanup(End::Stop).await
}

#[tokio::test]
async fn an_incoming_hangup_during_peer_setup_releases_the_peer_without_stopping_the_application(
) -> Result<(), Error> {
    setup_cleanup(End::Hangup).await
}

#[tokio::test]
async fn incoming_handoff_during_peer_setup_releases_the_peer_and_preserves_the_destination(
) -> Result<(), Error> {
    setup_cleanup(End::Handoff).await
}

enum End {
    Stop,
    Hangup,
    Handoff,
}

async fn setup_cleanup(end: End) -> Result<(), Error> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| Error::Invalid("example test configuration"))?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
    let mut destination = client
        .subscribe(&ChannelId::try_from("destination".to_owned())?)
        .await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut demo = tokio::spawn(super::run(
        client.clone(),
        ChannelId::try_from("calls".to_owned())?,
        "PJSIP/bob".into(),
        async { stopped.await.map_err(|_| Error::Closed) },
    ));
    let operation = Wait::Ready.bounded(async {
        loop {
            if client.application("calls").await.is_ok() {
                break;
            }

        }
        let incoming = controller
            .incoming(IncomingCall {
                id: ChannelId::try_from("incoming".to_owned())?,
                endpoint: "alice".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "calls".into(),
                    arguments: Vec::new(),
                },
            })
            .await
            .map_err(|_| Error::Closed)?;
        loop {
            match effects.recv().await {
                Some(Effect::Answer { key }) => {

                    assert!(client.bridges().await?.is_empty());
                    controller
                        .signal(Signal::Milestone {
                            key,
                            milestone: Milestone::Answered,
                        })
                        .await
                        .map_err(|_| Error::Closed)?;
                }
                Some(Effect::Start { .. }) => break,
                Some(_) => {}
                None => return Err(Error::Closed),
            }
        }
        assert_eq!(client.channels().await?.len(), 2);
        assert_eq!(client.bridges().await?.len(), 1);
        if !matches!(end, End::Stop) {
            match end {
                End::Hangup => controller.signal(Signal::Milestone { key:incoming.clone(), milestone:Milestone::Ended }).await.map_err(|_| Error::Closed)?,
                End::Handoff => {
                    controller.execute(Credentials::new("app".into(), "test-password".into()).map_err(|_| Error::Closed)?, None,
                        Action::Move { channel:incoming.id.clone(), app:"destination".into(), args:Vec::new() }).await.map_err(|_| Error::Closed)?;
                    loop {
                        let event = destination.next_event().await?;
                        if matches!(event.kind, rusterisk_client::api::EventKind::StasisStart { channel, .. } if channel.id == incoming.id) { break; }
                    }
                }
                End::Stop => return Err(Error::Invalid("expected an incoming transition")),
            }
            loop {
                let remaining = client.channels().await?;
                let preserved = match end {
                    End::Hangup => remaining.is_empty(),
                    End::Handoff => remaining.len() == 1 && remaining[0].id == incoming.id,
                    End::Stop => false,
                };
                if preserved && client.bridges().await?.is_empty() {
                    break;
                }

            }
            assert!(!demo.is_finished());
        }
        Ok::<(), Error>(())
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    let stop_result = stop.send(()).map_err(|_| Error::Closed);
    let joined = match Wait::Ready.bounded(&mut demo).await {
        Ok(result) => result.map_err(|_| Error::Closed).and_then(|result| result),
        Err(_) => {
            demo.abort();
            let _joined = demo.await;
            Err(Error::Timeout)
        }
    };
    let resources = async {
        if matches!(end, End::Handoff) {
            if client.channels().await?.len() != 1 {
                return Err(Error::Invalid(
                    "handoff must preserve the destination channel",
                ));
            }
            assert_eq!(
                client
                    .channel(&ChannelId::try_from("incoming".to_owned())?)
                    .await?
                    .state,
                rusterisk_client::api::ChannelState::Up
            );
            destination
                .adopt(&ChannelId::try_from("incoming".to_owned())?)
                .await?
                .close()
                .await?;
        }
        assert!(client.channels().await?.is_empty());
        assert!(client.bridges().await?.is_empty());
        Ok::<(), Error>(())
    }
    .await;
    let destination = destination.shutdown().await;
    controller.shutdown();
    let runner = runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed);
    server.abort();
    assert!(server.await.is_err());
    let mut failures = Vec::with_capacity(6);
    for outcome in [
        operation,
        stop_result,
        joined,
        resources,
        runner,
        destination,
    ] {
        if let Err(error) = outcome {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::Cleanup(failures.join("; ")))
    }
}
