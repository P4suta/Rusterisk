use futures_util::StreamExt;
use rusterisk_config::Config;
use rusterisk_server::runtime::Server;
use rusterisk_server::ControlError;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn server_shutdown_waits_for_event_sockets_and_releases_every_listener(
) -> Result<(), ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    let data = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(config, 4, data.path().to_owned()).await?;
    assert!(matches!(
        rusterisk_storage::Store::open(data.path()),
        Err(rusterisk_storage::StorageError::InUse)
    ));
    let address = server.local_addr()?;
    let sip = server.sip_listeners()?;
    let controller = server.controller();
    let task = tokio::spawn(server.run());
    let result: Result<(), ControlError> = async {
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(failure)?
            .get(format!("http://{address}/ari/asterisk/ping"))
            .basic_auth("app", Some("test-password"))
            .send()
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let mut request = format!("ws://{address}/ari/events?app=app")
            .into_client_request()
            .map_err(failure)?;
        request.headers_mut().insert(
            "Authorization",
            "Basic YXBwOnRlc3QtcGFzc3dvcmQ=".parse().map_err(failure)?,
        );
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "ari".parse().map_err(failure)?);
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(failure)?;
        controller.shutdown();
        let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .map_err(failure)?;
        assert!(matches!(
            message,
            None | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
        ));
        Ok(())
    }
    .await;
    controller.shutdown();
    tokio::time::timeout(Duration::from_secs(6), task)
        .await
        .map_err(failure)?
        .map_err(failure)??;
    result?;
    let http = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure)?;
    assert_eq!(http.local_addr().map_err(failure)?, address);
    for (_, address) in sip {
        let socket = tokio::net::UdpSocket::bind(address)
            .await
            .map_err(failure)?;
        assert_eq!(socket.local_addr().map_err(failure)?, address);
    }
    assert!(rusterisk_storage::Store::open(data.path()).is_ok());
    Ok(())
}

#[tokio::test]
async fn listener_startup_failure_joins_storage_before_returning() -> Result<(), ControlError> {
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    config.http.bind = occupied.local_addr().map_err(failure)?;
    let data = tempfile::tempdir().map_err(failure)?;
    assert!(Server::bind(config, 4, data.path().to_owned())
        .await
        .is_err());
    assert!(rusterisk_storage::Store::open(data.path()).is_ok());
    Ok(())
}

#[tokio::test]
async fn server_shutdown_flushes_history_before_releasing_its_store() -> Result<(), ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "[internal]\nexten => 100,1,Hangup()\n",
    )
    .map_err(failure)?;
    config.http.bind.set_port(0);
    let directory = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(config, 4, directory.path().to_owned()).await?;
    let controller = server.controller();
    let task = tokio::spawn(server.run());
    let result = controller
        .incoming(rusterisk_server::IncomingCall {
            id: rusterisk_api::ChannelId::try_from("short".to_owned())?,
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: rusterisk_server::IncomingTarget::Dialplan,
        })
        .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    result?;
    let store = rusterisk_storage::Store::open(directory.path()).map_err(failure)?;
    let records = store.call_records(10).map_err(failure)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].channel, "short");
    assert_eq!(
        records[0].outcome,
        rusterisk_storage::CallOutcome::Completed
    );
    Ok(())
}
