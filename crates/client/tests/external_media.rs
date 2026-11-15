use rusterisk_client::{
    api::{Channel, ChannelId, ExternalRtpTarget, StaticRtpCodec},
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn external_media_requests_use_the_public_contract_and_keep_local_transport_variables(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route("/ari/channels/externalMedia", axum::routing::post(|axum::extract::Query(parameters): axum::extract::Query<BTreeMap<String, String>>| async move {
        for (name, value) in [("channelId", "external"), ("app", "calls"), ("external_host", "127.0.0.1:12000"), ("format", "ulaw"), ("encapsulation", "rtp"), ("transport", "udp"), ("connection_type", "client"), ("direction", "both"), ("data", "media,agent")] { assert_eq!(parameters.get(name).map(String::as_str), Some(value)); }
        assert_eq!(parameters.len(), 9);
        axum::Json(serde_json::json!({"id":"external", "name":"UnicastRTP/peer", "state":"Down", "accountcode":"", "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""}, "dialplan":{"context":"default", "exten":"s", "priority":1, "app_name":"", "app_data":""}, "creationtime":"now", "language":"en", "channelvars":{"UNICASTRTP_LOCAL_ADDRESS":"127.0.0.1", "UNICASTRTP_LOCAL_PORT":"10000"}}))
    }));
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let target = ExternalRtpTarget::new(
            "127.0.0.1:12000"
                .parse()
                .map_err(|_| Error::Invalid("peer address"))?,
            StaticRtpCodec::Pcmu,
        )
        .map_err(|_| Error::Invalid("peer address"))?;
        let channel: Channel = client
            .external_rtp(
                &ChannelId::try_from("external".to_owned())?,
                "calls",
                target,
                "media,agent",
            )
            .await?;
        assert_eq!(channel.id.as_str(), "external");
        assert_eq!(
            channel
                .channelvars
                .as_ref()
                .and_then(|variables| variables.get("UNICASTRTP_LOCAL_PORT"))
                .map(String::as_str),
            Some("10000")
        );
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
