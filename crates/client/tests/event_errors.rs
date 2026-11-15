use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use rusterisk_client::{api::ChannelId, Client, Error};
use std::{future::IntoFuture, time::Duration};

async fn malformed_event(mut socket: WebSocket) {
    let result = socket
        .send(Message::Text("{\"type\":\"UnknownEvent\"}".into()))
        .await;
    assert!(result.is_ok());
    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(Message::Text(_) | Message::Binary(_) | Message::Ping(_) | Message::Pong(_)) => {}
        }
    }
}

#[tokio::test]
async fn event_consumers_receive_the_driver_error_when_decoding_stops_the_session(
) -> Result<(), Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = axum::Router::new().route(
        "/ari/events",
        axum::routing::get(|upgrade: WebSocketUpgrade| async move {
            upgrade.protocols(["ari"]).on_upgrade(malformed_event)
        }),
    );
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let result = async {
        let client = Client::new(&format!("http://{address}"), "app", "test-password")?;
        let mut session = client
            .subscribe(&ChannelId::try_from("calls".to_owned())?)
            .await?;
        let error = tokio::time::timeout(Duration::from_secs(2), session.next_event())
            .await
            .map_err(|_| Error::Timeout)?;
        assert!(matches!(error, Err(Error::Json(_))));
        assert!(matches!(session.next_event().await, Err(Error::Closed)));
        Ok(())
    }
    .await;
    server.abort();
    assert!(server.await.is_err());
    result
}
