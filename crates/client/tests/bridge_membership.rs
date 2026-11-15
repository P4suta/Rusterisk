use axum::extract::{Path, Query};
use rusterisk_client::{api::ChannelId, Client, Error};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn bridge_membership_uses_bounded_channel_lists_and_public_operations() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route(
        "/ari/bridges/{bridge}/{operation}",
        axum::routing::post(
            |Path((bridge, operation)): Path<(String, String)>,
             Query(query): Query<BTreeMap<String, String>>| async move {
                assert_eq!(bridge, "mix");
                assert!(matches!(operation.as_str(), "addChannel" | "removeChannel"));
                assert_eq!(query.len(), 1);
                assert_eq!(query.get("channel").map(String::as_str), Some("left,right"));
                axum::http::StatusCode::NO_CONTENT
            },
        ),
    );
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let bridge = ChannelId::try_from("mix".to_owned())?;
        let channels = [
            ChannelId::try_from("left".to_owned())?,
            ChannelId::try_from("right".to_owned())?,
        ];
        client.add_channels(&bridge, &channels).await?;
        client.remove_channels(&bridge, &channels).await?;
        assert!(matches!(
            client.add_channels(&bridge, &[]).await,
            Err(Error::Invalid(_))
        ));
        let excessive: Vec<_> = (0..33).map(|_| channels[0].clone()).collect();
        assert!(matches!(
            client.remove_channels(&bridge, &excessive).await,
            Err(Error::Capacity)
        ));
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
