use rusterisk_client::{
    api::{ApiError, ChannelId},
    Client, Error,
};
use std::{future::IntoFuture, time::Duration};

fn rejection<T>(result: Result<T, Error>) -> Result<(u16, ApiError), Error> {
    match result {
        Err(Error::Api { status, message }) => Ok((status, message)),
        Err(error) => Err(error),
        Ok(_) => Err(Error::Invalid("expected a rejected resource query")),
    }
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn resource_queries_and_app_subscriptions_match_the_reference() -> Result<(), Error> {
    let reference = std::env::var("RUSTERISK_REFERENCE_URL")
        .map_err(|_| Error::Invalid("reference URL is required"))?;
    let reference = Client::new(&reference, "app", "test-password")?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if reference.ping().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| Error::Timeout)?;
    let configuration = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &include_str!("../../../tests/interop/asterisk/pjsip.conf")
            .replace("0.0.0.0:5060", "127.0.0.1:0"),
        "",
        "",
    )
    .map_err(|_| Error::Invalid("reference configuration"))?;
    let (controller, runner, _effects) = rusterisk_server::Controller::new(configuration, 4);
    let runner = tokio::spawn(runner.run());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(
        axum::serve(listener, rusterisk_server::http::router(controller.clone())).into_future(),
    );
    let result = async {
        let actual = Client::new(&format!("http://{address}"), "app", "test-password")?;
        assert_eq!(actual.channels().await?, reference.channels().await?);
        assert_eq!(
            actual.applications().await?,
            reference.applications().await?
        );
        let mut expected = reference.endpoints().await?;
        expected.sort_by(|left, right| left.resource.cmp(&right.resource));
        assert_eq!(actual.endpoints().await?, expected);
        for name in ["phone", "phone-extra"] {
            assert_eq!(
                actual.endpoint("PJSIP", name).await?,
                reference.endpoint("PJSIP", name).await?
            );
        }
        for technology in ["PJSIP", "pjsip", "Pjsip"] {
            match reference.endpoints_by_technology(technology).await {
                Ok(mut expected) => {
                    expected.sort_by(|left, right| left.resource.cmp(&right.resource));
                    assert_eq!(actual.endpoints_by_technology(technology).await?, expected);
                }
                Err(Error::Api { status, message }) => {
                    assert_eq!(
                        rejection(actual.endpoints_by_technology(technology).await)?,
                        (status, message)
                    );
                }
                Err(error) => return Err(error),
            }
            match reference.endpoint(technology, "phone").await {
                Ok(expected) => assert_eq!(actual.endpoint(technology, "phone").await?, expected),
                Err(Error::Api { status, message }) => {
                    assert_eq!(
                        rejection(actual.endpoint(technology, "phone").await)?,
                        (status, message)
                    );
                }
                Err(error) => return Err(error),
            }
        }
        assert_eq!(
            rejection(actual.endpoint("PJSIP", "missing").await)?,
            rejection(reference.endpoint("PJSIP", "missing").await)?
        );
        assert_eq!(
            rejection(actual.endpoints_by_technology("unknown").await)?,
            rejection(reference.endpoints_by_technology("unknown").await)?
        );
        let app = ChannelId::try_from("compat-app".to_owned())?;
        let actual_session = actual.subscribe(&app).await?;
        let reference_session = reference.subscribe(&app).await?;
        assert_eq!(
            actual.application(app.as_str()).await?,
            reference.application(app.as_str()).await?
        );
        assert_eq!(
            rejection(actual.application("missing").await)?,
            rejection(reference.application("missing").await)?
        );
        reference_session.shutdown().await?;
        actual_session.shutdown().await?;
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
