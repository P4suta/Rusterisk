use rusterisk_client::{
    api::{ChannelId, EventKind},
    Client, Error,
};
use std::{future::IntoFuture, time::Duration};

#[tokio::test]
async fn subscription_is_ready_before_return_and_its_driver_receives_while_the_caller_waits(
) -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|_| Error::Invalid("test configuration"))?;
    let (controller, runner, _effects) = rusterisk_server::Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let mut session = client.subscribe(&ChannelId::try_from("app".to_owned())?).await?;
        client.create_bridge(&rusterisk_client::BridgeId::try_from("first".to_owned())?, "Conversation").await?;
        let event = tokio::time::timeout(Duration::from_secs(2), session.next_event()).await.map_err(|_| Error::Timeout)??;
        assert!(matches!(event.kind, EventKind::BridgeCreated { bridge } if bridge.id.as_str() == "first"));
        session.shutdown().await?;
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
