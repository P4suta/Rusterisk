use axum::extract::ws::WebSocketUpgrade;
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_media::{mulaw_decode, mulaw_encode, MediaError};
use rusterisk_server::{
    media::{MediaHub, MediaNotice},
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
        channel: format!("WebSocket/INCOMING/{}", key.id),
        format: MediaFormat::Pcmu,
        control_format: MediaControlFormat::Json,
        direction: MediaDirection::Both,
        auto_answer: true,
        channel_variables: Default::default(),
    }
}

#[tokio::test]
async fn websocket_peers_exchange_known_audio_through_the_media_hub() -> Result<(), MediaError> {
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
    let (media, hub, mut notices) = MediaHub::new(4);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let (mut left, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
            .await
            .map_err(failure)?;
        let left_key = key("left", 1)?;
        media
            .attach_websocket(
                left_key.clone(),
                sockets.recv().await.ok_or(MediaError::UnknownStream)?,
                options(&left_key),
            )
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
            MediaSocket::receive(&mut left).await?,
            Some(SocketMessage::Text(_))
        ));
        assert!(matches!(
            MediaSocket::receive(&mut right).await?,
            Some(SocketMessage::Text(_))
        ));
        let mut answers = Vec::with_capacity(2);
        while answers.len() < 2 {
            if let Some(MediaNotice::Answer { key }) = notices.recv().await {
                answers.push(key);
            }
        }
        assert!(answers.contains(&left_key) && answers.contains(&right_key));
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
        let receive_left = async {
            loop {
                if let Some(SocketMessage::Binary(bytes)) = MediaSocket::receive(&mut left).await? {
                    if bytes.len() == 160
                        && bytes
                            .iter()
                            .all(|value| (i32::from(mulaw_decode(*value)) + 7000).abs() < 300)
                    {
                        return Ok::<(), MediaError>(());
                    }
                }
            }
        };
        let receive_right = async {
            loop {
                if let Some(SocketMessage::Binary(bytes)) = MediaSocket::receive(&mut right).await?
                {
                    if bytes.len() == 160
                        && bytes
                            .iter()
                            .all(|value| (i32::from(mulaw_decode(*value)) - 4000).abs() < 300)
                    {
                        return Ok::<(), MediaError>(());
                    }
                }
            }
        };
        tokio::try_join!(receive_left, receive_right)?;
        MediaSocket::send(&mut left, SocketMessage::Close).await?;
        loop {
            match MediaSocket::receive(&mut left).await? {
                Some(SocketMessage::Close) => break,
                Some(
                    SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_),
                ) => {}
                Some(SocketMessage::Ping(bytes)) => {
                    MediaSocket::send(&mut left, SocketMessage::Pong(bytes)).await?
                }
                None => return Err(MediaError::InvalidPacket),
            }
        }
        loop {
            if let Some(MediaNotice::Stopped { key, result }) = notices.recv().await {
                assert_eq!(key, left_key);
                result?;
                break;
            }
        }
        media.release(right_key)?;
        Ok::<(), MediaError>(())
    })
    .await
    .map_err(failure);
    stop.send_replace(true);
    task.await.map_err(failure)??;
    http.abort();
    assert!(http.await.is_err());
    result?
}
