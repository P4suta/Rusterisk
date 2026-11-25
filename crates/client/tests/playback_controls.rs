use rusterisk_client::{api::ApiError, Client, Error, Playback, PlaybackId};
use std::{collections::BTreeMap, future::IntoFuture};

#[tokio::test]
async fn playback_queries_and_controls_preserve_wire_operations_and_errors() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (observed, mut commands) = tokio::sync::mpsc::channel(2);
    let app = axum::Router::new()
        .route("/ari/playbacks/announcement", axum::routing::get(|| async { axum::Json(serde_json::json!({"id":"announcement", "media_uri":"sound:digits/one", "target_uri":"channel:phone", "language":"en", "state":"playing"})) }).delete(|| async { axum::http::StatusCode::NO_CONTENT }))
        .route("/ari/playbacks/missing", axum::routing::get(|| async { (axum::http::StatusCode::NOT_FOUND, axum::Json(ApiError {message:"Playback not found".into()})) }))
        .route("/ari/playbacks/announcement/control", axum::routing::post(move |axum::extract::Query(query): axum::extract::Query<BTreeMap<String,String>>| { let observed = observed.clone(); async move {
            assert_eq!(query.len(),1);
            let operation = query.get("operation").ok_or(axum::http::StatusCode::BAD_REQUEST)?.clone();
            observed.send(operation).await.map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
            Ok::<_,axum::http::StatusCode>(axum::http::StatusCode::NO_CONTENT)
        }}));
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"),"app","password")?;
        let id = PlaybackId::try_from("announcement".to_owned())?;
        let playback: Playback = client.playback(&id).await?;
        assert_eq!(playback.state, rusterisk_client::api::PlaybackState::Playing);
        client.pause_playback(&id).await?;
        client.resume_playback(&id).await?;
        assert_eq!(commands.recv().await.as_deref(),Some("pause"));
        assert_eq!(commands.recv().await.as_deref(),Some("unpause"));
        client.restart_playback(&id).await?;
        if commands.recv().await.as_deref() != Some("restart") { return Err(Error::Invalid("incorrect restart request")); }
        client.forward_playback(&id).await?;
        if commands.recv().await.as_deref() != Some("forward") { return Err(Error::Invalid("incorrect forward request")); }
        client.reverse_playback(&id).await?;
        if commands.recv().await.as_deref() != Some("reverse") { return Err(Error::Invalid("incorrect reverse request")); }
        client.stop_playback(&id).await?;
        assert!(matches!(client.playback(&PlaybackId::try_from("missing".to_owned())?).await, Err(Error::Api { status:404, message }) if message.message == "Playback not found"));
        Ok(())
    }.await;
    serving.abort();
    assert!(serving.await.is_err_and(|error| error.is_cancelled()));
    result
}
