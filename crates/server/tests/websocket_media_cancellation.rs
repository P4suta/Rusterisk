use axum::extract::ws::WebSocketUpgrade;
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_media::MediaError;
use rusterisk_server::{
    media::MediaHub,
    media_websocket::{MediaSocket, SocketMessage},
    Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::future::IntoFuture;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn create(connection: &str) -> Result<Action, ControlError> {
    Ok(Action::ExternalWebSocket {
        id: id("external")?,
        app: "calls".into(),
        connection_id: id(connection)?,
        format: MediaFormat::Pcmu,
        control_format: Some(MediaControlFormat::Json),
        direction: MediaDirection::Both,
        auto_answer: false,
        data: String::new(),
    })
}

#[tokio::test]
async fn released_control_claims_reject_late_sockets_and_stop_attached_media(
) -> Result<(), ControlError> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
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
    let credentials = Credentials::new("app".into(), "test-password".into())?;
    let result = Wait::Ready
        .bounded(async {
            let _subscription = controller
                .subscribe(credentials.clone(), None, "calls".into())
                .await?;
            controller
                .execute(credentials.clone(), None, create("stale")?)
                .await?;
            let stale = controller
                .claim_media_connection(id("stale")?, None)
                .await?;
            controller
                .execute(credentials.clone(), None, Action::Hangup(id("external")?))
                .await?;
            while !stale.lease.is_released() {}
            let (_peer, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
                .await
                .map_err(failure)?;
            assert!(matches!(
                media
                    .attach_claimed_websocket(
                        stale,
                        sockets.recv().await.ok_or(ControlError::Capacity)?
                    )
                    .await,
                Err(MediaError::UnknownStream)
            ));
            controller
                .execute(credentials, None, create("active")?)
                .await?;
            let active = controller
                .claim_media_connection(id("active")?, None)
                .await?;
            let (mut peer, _) = tokio_tungstenite::connect_async(format!("ws://{address}/media"))
                .await
                .map_err(failure)?;
            media
                .attach_claimed_websocket(
                    active,
                    sockets.recv().await.ok_or(ControlError::Capacity)?,
                )
                .await
                .map_err(failure)?;
            assert!(matches!(
                MediaSocket::receive(&mut peer).await.map_err(failure)?,
                Some(SocketMessage::Text(_))
            ));
            controller.shutdown();
            loop {
                match MediaSocket::receive(&mut peer).await.map_err(failure)? {
                    Some(SocketMessage::Close) => break,
                    Some(
                        SocketMessage::Binary(_) | SocketMessage::Text(_) | SocketMessage::Pong(_),
                    ) => {}
                    Some(SocketMessage::Ping(bytes)) => {
                        MediaSocket::send(&mut peer, SocketMessage::Pong(bytes))
                            .await
                            .map_err(failure)?
                    }
                    None => return Err(ControlError::Unsupported),
                }
            }
            Ok::<(), ControlError>(())
        })
        .await
        .map_err(failure);
    controller.shutdown();
    runner.await.map_err(failure)??;
    stop.send_replace(true);
    hub.await.map_err(failure)?.map_err(failure)?;
    http.abort();
    assert!(http.await.is_err());
    result?
}
