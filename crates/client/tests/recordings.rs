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

#[tokio::test]
async fn recording_download_streams_past_json_limits_and_preserves_error_responses(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = axum::Router::new()
        .route(
            "/ari/recordings/stored/saved/file",
            axum::routing::get(|| async {
                axum::body::Body::from_stream(futures_util::stream::iter(
                    (0..256).map(|index| Ok::<_, std::io::Error>(vec![index as u8; 8192])),
                ))
            }),
        )
        .route(
            "/ari/recordings/stored/missing/file",
            axum::routing::get(|| async {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    axum::Json(api::ApiError {
                        message: "Recording not found".into(),
                    }),
                )
            }),
        )
        .route(
            "/ari/recordings/stored/huge/file",
            axum::routing::get(|| async {
                (
                    [("content-length", "1073741869")],
                    axum::body::Body::from_stream(futures_util::stream::pending::<
                        Result<Vec<u8>, std::io::Error>,
                    >()),
                )
            }),
        );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "password")?;
        let mut bytes = Vec::new();
        assert_eq!(client.download_stored_recording(&api::ChannelId::try_from("saved".to_owned())?, &mut bytes).await?, 256 * 8192);
        assert_eq!(bytes.len(), 256 * 8192);
        for (index, chunk) in bytes.as_chunks::<8192>().0.iter().enumerate() {
            assert!(chunk.iter().all(|byte| *byte == index as u8));
        }
        let mut output = Vec::new();
        assert!(matches!(client.download_stored_recording(&api::ChannelId::try_from("missing".to_owned())?, &mut output).await,
            Err(Error::Api { status: 404, message }) if message.message == "Recording not found"));
        assert!(output.is_empty());
        assert!(matches!(client.download_stored_recording(&api::ChannelId::try_from("huge".to_owned())?, &mut output).await, Err(Error::Capacity)));
        assert!(output.is_empty());
        let (mut broken, peer) = tokio::io::duplex(1);
        drop(peer);
        assert!(matches!(
            client.download_stored_recording(&api::ChannelId::try_from("saved".to_owned())?, &mut broken).await,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe
        ));
        Ok(())
    }.await;
    server.abort();
    assert!(server.await.is_err());
    result
}
