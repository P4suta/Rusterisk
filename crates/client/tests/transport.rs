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
async fn request_limits_are_checked_before_connecting_to_the_server() -> Result<(), Error> {
    let client = Client::new("http://127.0.0.1:9", "app", "password")?;
    let id = ChannelId::try_from("conversation".to_owned())?;
    for name in ["A".repeat(4097), "&".repeat(1400)] {
        assert!(matches!(
            client.create_bridge(&id, &name).await,
            Err(Error::Capacity)
        ));
    }
    assert!(matches!(
        Client::new(
            &format!("https://{}.example", "a".repeat(2050)),
            "app",
            "password"
        ),
        Err(Error::Invalid(_))
    ));
    Ok(())
}

#[tokio::test]
async fn typed_client_authenticates_and_roundtrips_server_resources() -> Result<(), Error> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[dynamic]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\nallow=ulaw\n",
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
        let endpoint = client.endpoint("PJSIP", "phone").await?;
        assert_eq!(
            endpoint.state,
            Some(rusterisk_client::api::EndpointState::Offline)
        );
        assert_eq!(client.endpoints().await?, std::slice::from_ref(&endpoint));
        assert_eq!(client.endpoints_by_technology("PJSIP").await?, [endpoint]);
        assert!(matches!(
            client.endpoints_by_technology("unknown").await,
            Err(Error::Api { status: 404, .. })
        ));
        assert!(matches!(
            client.endpoint("PJSIP", "missing").await,
            Err(Error::Api { status: 404, .. })
        ));
        assert!(matches!(
            client.endpoint("PJSIP", "../phone").await,
            Err(Error::Id(_))
        ));
        assert!(client.applications().await?.is_empty());
        let session = client
            .subscribe(&ChannelId::try_from("calls".to_owned())?)
            .await?;
        let application = client.application("calls").await?;
        assert_eq!(application.name, "calls");
        assert!(application.channel_ids.is_empty());
        assert_eq!(client.applications().await?, [application]);
        assert!(matches!(
            client.application("missing").await,
            Err(Error::Api { status: 404, .. })
        ));
        assert!(matches!(
            client.application("../calls").await,
            Err(Error::Id(_))
        ));
        session.shutdown().await?;
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
