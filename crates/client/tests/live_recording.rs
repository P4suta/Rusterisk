use rusterisk_client::{api, Client, Error};
use std::future::IntoFuture;

#[tokio::test]
async fn recording_creation_preserves_typed_options_and_target_paths() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (observed, mut requests) = tokio::sync::mpsc::channel(2);
    let app = axum::Router::new().route("/ari/{resource}/{id}/record", axum::routing::post(
        move |axum::extract::Path((resource, id)): axum::extract::Path<(String,String)>,
              axum::Json(body): axum::Json<serde_json::Value>| {
            let observed = observed.clone();
            async move {
                let target = match resource.as_str() { "channels" => format!("channel:{id}"), "bridges" => format!("bridge:{id}"), _ => return Err(axum::http::StatusCode::NOT_FOUND) };
                observed.send(body).await.map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                Ok::<_,axum::http::StatusCode>((axum::http::StatusCode::CREATED, axum::Json(serde_json::json!({"name":"known", "format":"wav", "state":"queued", "target_uri":target}))))
            }
        }
    ));
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let target = api::ChannelId::try_from("phone".to_owned())?;
        let bridge_id = rusterisk_client::BridgeId::try_from("phone".to_owned())?;
        let name = rusterisk_client::RecordingName::try_from("known".to_owned())?;
        let options = api::RecordingOptions { if_exists:api::RecordingIfExists::Append, max_duration_seconds:12.try_into().map_err(|_| Error::Invalid("duration"))?, max_silence_seconds:3.try_into().map_err(|_| Error::Invalid("silence"))?, beep:true, terminate_on:api::RecordingTermination::Hash };
        let channel = client.record_channel(&target, &name, "wav", options).await?;
        let bridge = client.record_bridge(&bridge_id, &name, "wav", options).await?;
        assert_eq!(channel.target_uri, "channel:phone");
        assert_eq!(bridge.target_uri, "bridge:phone");
        for recording in [channel, bridge] {
            assert_eq!(recording.name, name);
            assert_eq!(recording.format, "wav");
            assert_eq!(recording.state, api::RecordingState::Queued);
            assert_eq!(recording.duration, None);
        }
        for _ in 0..2 {
            assert_eq!(requests.recv().await.ok_or(Error::Closed)?, serde_json::json!({"name":"known", "format":"wav", "maxDurationSeconds":12, "maxSilenceSeconds":3, "ifExists":"append", "beep":true, "terminateOn":"#"}));
        }
        assert!(matches!(client.record_channel(&target, &name, "", options).await, Err(Error::Invalid(_))));
        assert!(matches!(client.record_bridge(&bridge_id, &name, "wav\r\n", options).await, Err(Error::Invalid(_))));
        assert!(requests.try_recv().is_err());
        Ok::<(),Error>(())
    }.await;
    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    result
}
