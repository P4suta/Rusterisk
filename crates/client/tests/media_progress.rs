use rusterisk_client::{CallMilestone, Client, Error, Originate};
use rusterisk_server::{Controller, Effect, Milestone, Signal};
use std::{future::IntoFuture, time::Duration};

#[tokio::test]
async fn media_received_before_stasis_is_discovered_without_a_second_notification(
) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "",
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
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let session = client.subscribe(&"app".to_owned().try_into()?).await?;
        let mut call = session
            .originate(Originate::new("PJSIP/phone".into())?)
            .await?;
        call.wait(CallMilestone::Accepted, Duration::from_secs(2))
            .await?;
        let Effect::Start { key, .. } = effects.recv().await.ok_or(Error::Closed)? else {
            return Err(Error::Invalid("expected start effect"));
        };
        for milestone in [Milestone::MediaConnected, Milestone::Answered] {
            controller
                .signal(Signal::Milestone {
                    key: key.clone(),
                    milestone,
                })
                .await
                .map_err(|_| Error::Closed)?;
        }
        call.wait(CallMilestone::Stasis, Duration::from_secs(2))
            .await?;
        assert_eq!(
            client
                .variable(call.id(), "RUSTERISK_MEDIA_CONNECTED")
                .await?
                .value,
            "1"
        );
        call.wait(CallMilestone::MediaConnected, Duration::from_secs(2))
            .await?;
        call.close().await?;
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
