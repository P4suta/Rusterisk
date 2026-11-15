use rusterisk_client::{
    api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat, WebSocketMediaOptions},
    Client, Error,
};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn incoming_media_requests_use_typed_options_and_preserve_stasis_data() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route("/ari/channels/externalMedia", axum::routing::post(|axum::extract::Query(parameters): axum::extract::Query<BTreeMap<String, String>>| async move {
        for (name, value) in [("channelId", "external"), ("app", "calls"), ("format", "ulaw"), ("encapsulation", "none"), ("transport", "websocket"), ("connection_type", "server"), ("direction", "out"), ("data", "media,agent"), ("transport_data", "f(json)n")] { assert_eq!(parameters.get(name).map(String::as_str), Some(value)); }
        assert_eq!(parameters.len(), 9);
        axum::Json(serde_json::json!({"id":"external", "name":"WebSocket/INCOMING/channel", "state":"Down", "accountcode":"", "caller":{"name":"", "number":""}, "connected":{"name":"", "number":""}, "dialplan":{"context":"default", "exten":"s", "priority":1, "app_name":"", "app_data":""}, "creationtime":"now", "language":"en", "channelvars":{"MEDIA_WEBSOCKET_CONNECTION_ID":"connection", "MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE":"160"}}))
    }));
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let options = WebSocketMediaOptions {
            format: MediaFormat::Pcmu,
            control_format: MediaControlFormat::Json,
            direction: MediaDirection::Out,
            auto_answer: false,
        };
        let channel = client
            .external_websocket(
                &ChannelId::try_from("external".to_owned())?,
                "calls",
                options,
                "media,agent",
            )
            .await?;
        assert_eq!(channel.id.as_str(), "external");
        assert_eq!(
            channel
                .channelvars
                .ok_or(Error::Capacity)?
                .get("MEDIA_WEBSOCKET_CONNECTION_ID")
                .map(String::as_str),
            Some("connection")
        );
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
