use axum::extract::ws::WebSocketUpgrade;
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_media::{mulaw_decode, mulaw_encode, MediaError};
use rusterisk_server::{
    media::MediaHub,
    media_websocket::{MediaLinkOptions, MediaSocket, SocketMessage},
    CallKey,
};
use std::{future::IntoFuture, time::Duration};
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}
fn key(id: &str, generation: u64) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(id.to_owned()).map_err(failure)?,
        generation,
    })
}
fn options(key: &CallKey) -> MediaLinkOptions {
    MediaLinkOptions {
        connection_id: key.id.clone(),
        channel_id: key.id.clone(),
        channel: format!("WebSocket/agent/{}", key.id),
        format: MediaFormat::Pcmu,
        control_format: MediaControlFormat::Json,
        direction: MediaDirection::Both,
        auto_answer: true,
        channel_variables: Default::default(),
    }
}

async fn received(socket: &mut impl MediaSocket, expected: i32) -> Result<(), MediaError> {
    loop {
        match socket.receive().await? {
            Some(SocketMessage::Binary(bytes))
                if bytes.len() == 160
                    && bytes
                        .iter()
                        .all(|byte| (i32::from(mulaw_decode(*byte)) - expected).abs() < 300) =>
            {
                return Ok(())
            }
            Some(SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_)) => {}
            Some(SocketMessage::Ping(bytes)) => socket.send(SocketMessage::Pong(bytes)).await?,
            Some(SocketMessage::Close) | None => return Err(MediaError::UnknownStream),
        }
    }
}

async fn received_close(socket: &mut impl MediaSocket) -> Result<(), MediaError> {
    loop {
        match socket.receive().await? {
            Some(SocketMessage::Close) => return socket.flush().await,
            Some(SocketMessage::Ping(bytes)) => socket.send(SocketMessage::Pong(bytes)).await?,
            Some(SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_)) => {}
            None => return Err(MediaError::UnknownStream),
        }
    }
}

#[tokio::test]
async fn client_and_server_media_connections_exchange_audio_through_one_bridge(
) -> Result<(), MediaError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let (sender, mut sockets) = mpsc::channel(2);
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
    let http = tokio::spawn(axum::serve(listener, app).into_future());
    let (media, hub, _notices) = MediaHub::new(4);
    let (stop, stopped) = watch::channel(false);
    let hub = tokio::spawn(hub.run(stopped));
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let (left_socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let mut left = sockets.recv().await.ok_or(MediaError::UnknownStream)?;
        let left_key = key("left", 1)?;
        media
            .attach_client_websocket(left_key.clone(), left_socket, options(&left_key))
            .await?;
        let (mut right, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let right_key = key("right", 2)?;
        media
            .attach_websocket(
                right_key.clone(),
                sockets.recv().await.ok_or(MediaError::UnknownStream)?,
                options(&right_key),
            )
            .await?;
        assert!(matches!(
            left.receive().await?,
            Some(SocketMessage::Text(_))
        ));
        assert!(matches!(
            right.receive().await?,
            Some(SocketMessage::Text(_))
        ));
        media.bridge(
            1,
            [left_key.clone(), right_key.clone()].into_iter().collect(),
        )?;
        MediaSocket::send(
            &mut left,
            SocketMessage::Binary(vec![mulaw_encode(4000); 160].into()),
        )
        .await?;
        MediaSocket::send(
            &mut right,
            SocketMessage::Binary(vec![mulaw_encode(-7000); 160].into()),
        )
        .await?;
        tokio::try_join!(received(&mut left, -7000), received(&mut right, 4000))?;
        media.release(left_key)?;
        media.release(right_key)?;
        tokio::try_join!(received_close(&mut left), received_close(&mut right))?;
        Ok::<(), MediaError>(())
    })
    .await
    .map_err(failure);
    stop.send_replace(true);
    hub.await.map_err(failure)??;
    http.abort();
    assert!(http.await.is_err());
    result?
}
