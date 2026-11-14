use rusterisk_client::{api::ChannelId, Client, Error};
use std::future::IntoFuture;

#[test]
fn client_settings_reject_credential_urls_and_insecure_external_management() {
    for address in [
        "ftp://localhost",
        "http://example.com",
        "http://127.0.0.1.evil.test",
        "https://user:secret@example.com",
        "https://example.com/ari",
        "https://example.com/?token=secret",
        "https://example.com/#fragment",
    ] {
        assert!(
            matches!(
                Client::new(address, "app", "password"),
                Err(Error::Invalid(_))
            ),
            "{address}"
        );
    }
}

#[tokio::test]
async fn typed_client_authenticates_and_roundtrips_server_resources() -> Result<(), Error> {
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
        assert_eq!(client.ping().await?.ping, "pong");
        assert!(client.channels().await?.is_empty());
        let id = ChannelId::try_from("conversation".to_owned())?;
        let bridge = client.create_bridge(&id, "Conversation").await?;
        assert_eq!(bridge.id, id);
        assert_eq!(client.bridge(&id).await?, bridge);
        assert_eq!(client.bridges().await?, [bridge]);
        client.destroy_bridge(&id).await?;
        assert!(matches!(
            client.bridge(&id).await,
            Err(Error::Api { status: 404, .. })
        ));
        let wrong = Client::new(&format!("http://{address}"), "app", "wrong-password")?;
        assert!(matches!(
            wrong.channels().await,
            Err(Error::Api { status: 401, .. })
        ));
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

#[tokio::test]
async fn response_limits_apply_to_chunked_bodies_and_redirects_are_not_followed(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new()
        .route(
            "/ari/channels",
            axum::routing::get(|| async {
                axum::body::Body::from_stream(futures_util::stream::iter([
                    Ok::<_, std::io::Error>(vec![b' '; 600_000]),
                    Ok(vec![b' '; 600_000]),
                ]))
            }),
        )
        .route(
            "/ari/bridges",
            axum::routing::get(|| async { axum::response::Redirect::temporary("/unexpected") }),
        )
        .route("/unexpected", axum::routing::get(|| async { "[]" }));
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        assert!(matches!(client.channels().await, Err(Error::Capacity)));
        assert!(client.bridges().await.is_err());
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
