use rusterisk_client::{api, Client, Error};
use std::future::IntoFuture;

fn saved() -> api::StoredRecording {
    api::StoredRecording {
        name: "saved".into(),
        format: "wav".into(),
    }
}

#[tokio::test]
async fn stored_recording_methods_use_the_public_contract_and_typed_models() -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new()
        .route(
            "/ari/recordings/stored",
            axum::routing::get(|| async { axum::Json([saved()]) }),
        )
        .route(
            "/ari/recordings/stored/saved",
            axum::routing::get(|| async { axum::Json(saved()) })
                .delete(|| async { axum::http::StatusCode::NO_CONTENT }),
        );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let name = api::ChannelId::try_from("saved".to_owned())?;
        assert_eq!(client.stored_recordings().await?, [saved()]);
        assert_eq!(client.stored_recording(&name).await?, saved());
        client.delete_stored_recording(&name).await?;
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
