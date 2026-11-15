use axum::extract::ws::{Message, WebSocketUpgrade};
use rusterisk_client::{
    api::{Channel, MediaCommand, MediaControlFormat, MediaEvent},
    Client, Error, MediaBytes, MediaMessage,
};
use std::{future::IntoFuture, time::Duration};

fn channel() -> Result<Channel, Error> {
    Ok(serde_json::from_str(
        r#"{"id":"channel","name":"WebSocket/INCOMING/channel","state":"Down","accountcode":"","caller":{"name":"","number":""},"connected":{"name":"","number":""},"dialplan":{"context":"default","exten":"s","priority":1,"app_name":"","app_data":""},"creationtime":"now","language":"en","channelvars":{"MEDIA_WEBSOCKET_CONNECTION_ID":"connection","MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE":"160"}}"#,
    )?)
}

async fn fixture() -> Result<
    (
        Client,
        tokio::sync::mpsc::Receiver<axum::extract::ws::WebSocket>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ),
    Error,
> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sender, accepted) = tokio::sync::mpsc::channel(1);
    let app = axum::Router::new().route("/media/connection", axum::routing::get(move |upgrade: WebSocketUpgrade| {
        let sender = sender.clone();
        async move {
            upgrade.protocols(["media"]).on_upgrade(async move |mut socket| {
                let sent = socket.send(Message::Text("MEDIA_START connection_id:connection channel:WebSocket/INCOMING/channel channel_id:channel format:ulaw optimal_frame_size:160 ptime:20".into())).await;
                assert!(sent.is_ok());
                assert!(sender.try_send(socket).is_ok());
            })
        }
    }));
    Ok((
        Client::new(&format!("http://{address}"), "app", "test-password")?,
        accepted,
        tokio::spawn(axum::serve(listener, app).into_future()),
    ))
}

#[tokio::test]
async fn cancelled_media_reads_preserve_the_connection_and_drop_releases_it() -> Result<(), Error> {
    let (client, mut accepted, server) = fixture().await?;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut media = client.connect_media(&channel()?, MediaControlFormat::PlainText).await?;
        let mut peer = accepted.recv().await.ok_or(Error::Closed)?;
        assert!(tokio::time::timeout(Duration::from_millis(10), media.next_message()).await.is_err());
        peer.send(Message::Binary(vec![0xd5; 160].into())).await.map_err(|_| Error::Closed)?;
        assert!(matches!(media.next_message().await?, Some(MediaMessage::Audio(bytes)) if bytes.as_ref() == [0xd5; 160]));
        drop(media);
        assert!(matches!(peer.recv().await, None | Some(Err(_))));
        Ok::<(), Error>(())
    }).await.map_err(|_| Error::Timeout);
    server.abort();
    assert!(server.await.is_err());
    result?
}

#[tokio::test]
async fn invalid_media_events_close_the_connection_before_returning_the_error() -> Result<(), Error>
{
    for text in ["UNKNOWN", "DTMF_END channel_id:another digit:1"] {
        let (client, mut accepted, server) = fixture().await?;
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let mut media = client
                .connect_media(&channel()?, MediaControlFormat::PlainText)
                .await?;
            let mut peer = accepted.recv().await.ok_or(Error::Closed)?;
            peer.send(Message::Text(text.into()))
                .await
                .map_err(|_| Error::Closed)?;
            assert!(matches!(media.next_message().await, Err(Error::Invalid(_))));
            assert!(media.next_message().await?.is_none());
            assert!(matches!(peer.recv().await, None | Some(Err(_))));
            Ok::<(), Error>(())
        })
        .await
        .map_err(|_| Error::Timeout);
        server.abort();
        assert!(server.await.is_err());
        result??;
    }
    Ok(())
}

#[tokio::test]
async fn media_sessions_negotiate_bearer_sockets_and_keep_audio_and_controls_typed(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (closed, observed) = tokio::sync::oneshot::channel();
    let (socket, mut accepted) = tokio::sync::mpsc::channel(1);
    let app = axum::Router::new().route(
        "/media/connection",
        axum::routing::get(
            move |headers: axum::http::HeaderMap, upgrade: WebSocketUpgrade| {
                let socket = socket.clone();
                async move {
                    assert!(!headers.contains_key("Authorization"));
                    upgrade
                        .protocols(["media"])
                        .on_upgrade(async move |stream| {
                            assert!(socket.try_send(stream).is_ok());
                        })
                }
            },
        ),
    );
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let mut exchange = tokio::spawn(async move {
        let mut socket = accepted.recv().await.ok_or(Error::Closed)?;
        socket.send(Message::Text("MEDIA_START connection_id:connection channel:WebSocket/INCOMING/channel channel_id:channel format:ulaw optimal_frame_size:160 ptime:20".into())).await.map_err(|_| Error::Closed)?;
        assert!(
            matches!(socket.recv().await, Some(Ok(Message::Text(text))) if text == "GET_STATUS")
        );
        socket.send(Message::Text("STATUS channel_id:channel queue_length:0 xon_level:800 xoff_level:900 queue_full:false bulk_media:false media_paused:false".into())).await.map_err(|_| Error::Closed)?;
        assert!(
            matches!(socket.recv().await, Some(Ok(Message::Binary(bytes))) if bytes.as_ref() == [0xff; 160])
        );
        socket
            .send(Message::Binary(vec![0xd5; 160].into()))
            .await
            .map_err(|_| Error::Closed)?;
        assert!(matches!(socket.recv().await, Some(Ok(Message::Close(_)))));
        futures_util::SinkExt::flush(&mut socket)
            .await
            .map_err(|_| Error::Closed)?;
        assert!(closed.send(()).is_ok());
        Ok::<(), Error>(())
    });
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let mut media = client.connect_media(&channel()?, MediaControlFormat::PlainText).await?;
        assert!(matches!(media.start_event(), MediaEvent::MediaStart { optimal_frame_size: 160, .. }));
        media.command(MediaCommand::GetStatus).await?;
        assert!(matches!(media.next_message().await?, Some(MediaMessage::Event(MediaEvent::Status { queue_length: 0, .. }))));
        media.send_audio(MediaBytes::from_static(&[0xff; 160])).await?;
        assert!(matches!(media.next_message().await?, Some(MediaMessage::Audio(bytes)) if bytes.as_ref() == [0xd5; 160]));
        media.close().await?;
        observed.await.map_err(|_| Error::Closed)?;
        Ok::<(), Error>(())
    }).await.map_err(|_| Error::Timeout);
    let exchanged = tokio::time::timeout(Duration::from_secs(1), &mut exchange).await;
    if exchanged.is_err() {
        exchange.abort();
        assert!(exchange.await.is_err());
    }
    server.abort();
    assert!(server.await.is_err());
    result??;
    exchanged
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Closed)?
}
