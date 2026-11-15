use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use rusterisk_api::{ChannelId, MediaControlFormat, MediaDirection, MediaFormat};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub,
    media_websocket::{MediaSocket, SocketMessage},
    signaling::Signaling,
    Action, ControlError, Controller, Credentials,
};
use std::time::Duration;
use tokio::sync::mpsc;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn exercise(
    client: &str,
    sockets: &mut mpsc::Receiver<WebSocket>,
    trusted: bool,
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?
    .with_websocket_configuration("", client)
    .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _subscription = controller.subscribe(credentials.clone(), None, "calls".into()).await?;
        let id = ChannelId::try_from("encrypted".to_owned())?;
        let result = controller.execute(credentials.clone(), None, Action::ExternalWebSocketClient { id: id.clone(), app: "calls".into(), client: "agent".into(), format: MediaFormat::Pcmu, control_format: Some(MediaControlFormat::Json), direction: MediaDirection::Both, auto_answer: true, data: String::new() }).await;
        if trusted {
            result?;
            let mut socket = sockets.recv().await.ok_or(ControlError::Capacity)?;
            assert!(matches!(MediaSocket::receive(&mut socket).await.map_err(failure)?, Some(SocketMessage::Text(text)) if text.contains("MEDIA_START")));
            controller.execute(credentials, None, Action::Hangup(id)).await?;
        } else {
            assert!(matches!(result, Err(ControlError::Adapter(message)) if message == "WebSocket connection failed"));
            assert!(sockets.try_recv().is_err());
        }
        Ok::<(), ControlError>(())
    }).await.map_err(failure);
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result?
}

#[tokio::test]
async fn tls_clients_require_a_trusted_certificate_and_a_matching_hostname(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = identity.cert.pem();
    let ca = directory.path().join("ca.pem");
    std::fs::write(&ca, &certificate).map_err(failure)?;
    let config = axum_server::tls_rustls::RustlsConfig::from_pem(
        certificate.into_bytes(),
        identity.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .map_err(failure)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let port = listener.local_addr().map_err(failure)?.port();
    let (sender, mut sockets) = mpsc::channel(1);
    let app = axum::Router::new().route(
        "/media",
        axum::routing::get(move |upgrade: WebSocketUpgrade| {
            let sender = sender.clone();
            async move {
                upgrade
                    .protocols(["media"])
                    .on_upgrade(async move |socket| {
                        assert!(sender.try_send(socket).is_ok());
                    })
            }
        }),
    );
    let handle = axum_server::Handle::new();
    let server = tokio::spawn(
        axum_server::from_tcp_rustls(listener.into_std().map_err(failure)?, config)
            .map_err(failure)?
            .handle(handle.clone())
            .serve(app.into_make_service()),
    );
    let result = async {
        let settings = format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=wss://localhost:{port}/media?token=private-query\nprotocols=media\ntls_enabled=yes\nreconnect_attempts=1\n");
        exercise(&settings, &mut sockets, false).await?;
        let trusted = format!("{settings}ca_list_file={}\n", ca.display());
        exercise(&trusted.replace("localhost:", "127.0.0.1:"), &mut sockets, false).await?;
        exercise(&trusted, &mut sockets, true).await
    }.await;
    handle.shutdown();
    server.await.map_err(failure)?.map_err(failure)?;
    result
}
