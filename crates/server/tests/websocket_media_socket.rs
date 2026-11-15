use axum::extract::ws::WebSocketUpgrade;
use rusterisk_media::MediaError;
use rusterisk_server::media_websocket::{MediaSocket, SocketMessage};
use std::{future::IntoFuture, time::Duration};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn concrete_client_and_server_sockets_preserve_media_message_types() -> Result<(), MediaError>
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (sender, mut sockets) = tokio::sync::mpsc::channel(1);
    let app = axum::Router::new().route(
        "/media",
        axum::routing::get(move |upgrade: WebSocketUpgrade| {
            let sender = sender.clone();
            async move {
                upgrade.on_upgrade(async move |socket| {
                    assert!(sender.try_send(socket).is_ok());
                })
            }
        }),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let result = async {
        let (mut client, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let mut accepted = tokio::time::timeout(Duration::from_secs(1), sockets.recv())
            .await
            .map_err(failure)?
            .ok_or(MediaError::Capacity)?;
        MediaSocket::send(&mut client, SocketMessage::Text("GET_STATUS".into())).await?;
        assert_eq!(
            MediaSocket::receive(&mut accepted).await?,
            Some(SocketMessage::Text("GET_STATUS".into()))
        );
        MediaSocket::send(
            &mut accepted,
            SocketMessage::Binary(axum::body::Bytes::from_static(&[1, 2, 3])),
        )
        .await?;
        assert_eq!(
            MediaSocket::receive(&mut client).await?,
            Some(SocketMessage::Binary(axum::body::Bytes::from_static(&[
                1, 2, 3
            ])))
        );
        MediaSocket::send(&mut client, SocketMessage::Close).await?;
        assert_eq!(
            MediaSocket::receive(&mut accepted).await?,
            Some(SocketMessage::Close)
        );
        MediaSocket::flush(&mut accepted).await?;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), MediaSocket::receive(&mut client))
                .await
                .map_err(failure)??,
            Some(SocketMessage::Close)
        );
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
