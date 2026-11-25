use rusterisk_config::{Config, TlsFiles};
use rusterisk_server::listener::HttpServer;
use rusterisk_server::{ControlError, Controller};
use rusterisk_test_support::Wait;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn config() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)
}

#[tokio::test]
async fn connection_admission_resumes_when_an_existing_socket_is_released(
) -> Result<(), ControlError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut config = config()?;
    config.http.bind.set_port(0);
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let server = HttpServer::bind(&config.http, controller.clone()).await?;
    let address = server.local_addr()?;
    let runner = tokio::spawn(runner.run());
    let server = tokio::spawn(server.run());
    let result: Result<(), ControlError> = async {
        let mut occupied = Vec::with_capacity(64);
        for _ in 0..64 { occupied.push(tokio::net::TcpStream::connect(address).await.map_err(failure)?); }
        let mut waiting = tokio::net::TcpStream::connect(address).await.map_err(failure)?;
        waiting.write_all(b"GET /ari/asterisk/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic YXBwOnRlc3QtcGFzc3dvcmQ=\r\nConnection: close\r\n\r\n").await.map_err(failure)?;
        let mut status = [0; 12];
        assert!(Wait::Quiet.bounded(waiting.read_exact(&mut status)).await.is_err());
        drop(occupied.pop());
        Wait::Ready.bounded(waiting.read_exact(&mut status)).await.map_err(failure)?.map_err(failure)?;
        assert_eq!(&status, b"HTTP/1.1 200");
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    server.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn silent_connections_expire_before_protocol_detection_can_hold_a_slot_forever(
) -> Result<(), ControlError> {
    use tokio::io::AsyncReadExt;
    let mut config = config()?;
    config.http.bind.set_port(0);
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let server = HttpServer::bind(&config.http, controller.clone()).await?;
    let address = server.local_addr()?;
    let runner = tokio::spawn(runner.run());
    let server = tokio::spawn(server.run());
    let result: Result<(), ControlError> = async {
        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .map_err(failure)?;
        let client = reqwest::Client::builder().no_proxy().build().map_err(failure)?;
        let response = Wait::Ready
            .bounded(client.get(format!("http://{address}/ari/asterisk/ping")).send())
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        let mut byte = [0; 1];
        assert!(matches!(socket.try_read(&mut byte), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::time::resume();
        let read = Wait::Responsive
            .bounded(socket.read(&mut byte))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(read, 0);
        Ok(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    server.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn encrypted_requests_use_a_trusted_certificate_and_shutdown_releases_the_listener(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = identity.cert.pem();
    let cert_path = directory.path().join("certificate.pem");
    let key_path = directory.path().join("key.pem");
    std::fs::write(&cert_path, &certificate).map_err(failure)?;
    std::fs::write(&key_path, identity.signing_key.serialize_pem()).map_err(failure)?;
    let mut config = config()?;
    config.http.bind.set_port(0);
    config.http.tls = Some(TlsFiles {
        certificate: cert_path.to_string_lossy().into(),
        private_key: key_path.to_string_lossy().into(),
    });
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let server = HttpServer::bind(&config.http, controller.clone()).await?;
    let address = server.local_addr()?;
    let runner = tokio::spawn(runner.run());
    let server = tokio::spawn(server.run());
    let result: Result<(), ControlError> = async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Wait::Io.duration())
            .tls_certs_only([
                reqwest::Certificate::from_pem(certificate.as_bytes()).map_err(failure)?
            ])
            .build()
            .map_err(failure)?;
        let url = format!("https://localhost:{}/ari/asterisk/ping", address.port());
        assert!(reqwest::Client::builder()
            .no_proxy()
            .timeout(Wait::Io.duration())
            .build()
            .map_err(failure)?
            .get(&url)
            .send()
            .await
            .is_err());
        let rejected = client.get(&url).send().await.map_err(failure)?;
        assert_eq!(rejected.status(), reqwest::StatusCode::UNAUTHORIZED);
        let response = client
            .get(&url)
            .basic_auth("app", Some("test-password"))
            .send()
            .await
            .map_err(failure)?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        let ping: rusterisk_api::AsteriskPing = response.json().await.map_err(failure)?;
        assert_eq!(ping.ping, "pong");
        for path in ["resources", "channels"] {
            let response = client
                .get(format!(
                    "https://localhost:{}/ari/api-docs/{path}.json",
                    address.port()
                ))
                .basic_auth("app", Some("test-password"))
                .send()
                .await
                .map_err(failure)?;
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let document: serde_json::Value = response.json().await.map_err(failure)?;
            assert_eq!(
                document["basePath"],
                format!("https://localhost:{}/ari", address.port())
            );
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    server.await.map_err(failure)??;
    result?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure)?;
    assert_eq!(listener.local_addr().map_err(failure)?, address);
    Ok(())
}

#[tokio::test]
async fn external_plaintext_and_invalid_encryption_material_are_refused() -> Result<(), ControlError>
{
    let config = config()?;
    let (controller, _runner, _effects) = Controller::new(config.clone(), 4);
    let mut http = config.http;
    http.bind = "0.0.0.0:0".parse().map_err(failure)?;
    assert!(HttpServer::bind(&http, controller.clone()).await.is_err());
    http.bind = "127.0.0.1:0".parse().map_err(failure)?;
    http.tls = Some(TlsFiles {
        certificate: "missing-certificate.pem".into(),
        private_key: "missing-key.pem".into(),
    });
    assert!(HttpServer::bind(&http, controller.clone()).await.is_err());
    let directory = tempfile::tempdir().map_err(failure)?;
    let cert = directory.path().join("invalid-certificate.pem");
    let key = directory.path().join("invalid-key.pem");
    std::fs::write(&cert, b"invalid certificate").map_err(failure)?;
    std::fs::write(&key, b"invalid private key").map_err(failure)?;
    http.tls = Some(TlsFiles {
        certificate: cert.to_string_lossy().into(),
        private_key: key.to_string_lossy().into(),
    });
    assert!(HttpServer::bind(&http, controller.clone()).await.is_err());
    std::fs::write(&cert, vec![0; 65537]).map_err(failure)?;
    assert!(matches!(
        HttpServer::bind(&http, controller).await,
        Err(ControlError::Invalid(_))
    ));
    Ok(())
}
