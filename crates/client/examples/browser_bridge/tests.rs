use rusterisk_client::{api::ChannelId, Client, Error};
use rusterisk_config::Config;
use rusterisk_server::{Controller, Effect, IncomingCall, IncomingTarget, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

#[tokio::test]
async fn stopping_during_peer_setup_releases_both_channels_and_the_bridge() -> Result<(), Error> {
    setup_cleanup(false).await
}

#[tokio::test]
async fn an_incoming_hangup_during_peer_setup_releases_the_peer_without_stopping_the_application(
) -> Result<(), Error> {
    setup_cleanup(true).await
}

async fn setup_cleanup(incoming_ends: bool) -> Result<(), Error> {
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
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut demo = tokio::spawn(super::run(
        client.clone(),
        ChannelId::try_from("calls".to_owned())?,
        "PJSIP/bob".into(),
        async { stopped.await.map_err(|_| Error::Closed) },
    ));
    let operation = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if client.application("calls").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
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
                    tokio::time::sleep(Duration::from_millis(100)).await;
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
        if incoming_ends {
            controller
                .signal(Signal::Milestone {
                    key: incoming,
                    milestone: Milestone::Ended,
                })
                .await
                .map_err(|_| Error::Closed)?;
            loop {
                if client.channels().await?.is_empty() && client.bridges().await?.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!demo.is_finished());
        }
        Ok::<(), Error>(())
    })
    .await
    .map_err(|_| Error::Timeout)
    .and_then(|result| result);
    let stop_result = stop.send(()).map_err(|_| Error::Closed);
    let joined = match tokio::time::timeout(Duration::from_secs(4), &mut demo).await {
        Ok(result) => result.map_err(|_| Error::Closed).and_then(|result| result),
        Err(_) => {
            demo.abort();
            let _joined = demo.await;
            Err(Error::Timeout)
        }
    };
    let resources = async {
        assert!(client.channels().await?.is_empty());
        assert!(client.bridges().await?.is_empty());
        Ok::<(), Error>(())
    }
    .await;
    controller.shutdown();
    let runner = runner
        .await
        .map_err(|_| Error::Closed)?
        .map_err(|_| Error::Closed);
    server.abort();
    assert!(server.await.is_err());
    let mut failures = Vec::with_capacity(5);
    for outcome in [operation, stop_result, joined, resources, runner] {
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
