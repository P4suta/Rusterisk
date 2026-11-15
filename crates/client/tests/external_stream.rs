use rusterisk_client::{
    api::{AudioSocketTarget, ChannelId},
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn external_stream_requests_use_a_typed_target_and_canonical_session_data(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route("/ari/channels/externalMedia", axum::routing::post(|axum::extract::Query(parameters): axum::extract::Query<BTreeMap<String, String>>| async move {
        for (name, value) in [("channelId", "external"), ("app", "calls"), ("external_host", "127.0.0.1:12000"), ("format", "slin"), ("encapsulation", "audiosocket"), ("transport", "tcp"), ("connection_type", "client"), ("direction", "both"), ("data", "07070707-0707-0707-0707-070707070707")] { assert_eq!(parameters.get(name).map(String::as_str), Some(value)); }
        assert_eq!(parameters.len(), 9);
        axum::Json(serde_json::json!({"id":"external", "name":"AudioSocket/peer", "state":"Down", "accountcode":"", "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""}, "dialplan":{"context":"default", "exten":"s", "priority":1, "app_name":"", "app_data":""}, "creationtime":"now", "language":"en"}))
    }));
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let target = AudioSocketTarget::new(
            "127.0.0.1:12000"
                .parse()
                .map_err(|_| Error::Invalid("peer address"))?,
            [7; 16],
        )
        .map_err(|_| Error::Invalid("peer address"))?;
        let channel = client
            .external_audio_socket(
                &ChannelId::try_from("external".to_owned())?,
                "calls",
                target,
            )
            .await?;
        assert_eq!(channel.id.as_str(), "external");
        assert!(channel.channelvars.is_none());
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
