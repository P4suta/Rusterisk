use super::{connect, failure, ControlError};
use futures_util::{SinkExt, StreamExt};
use rusterisk_config::Config;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{
    handshake::server::{Callback, ErrorResponse, Request, Response},
    http::HeaderValue,
    Message,
};

struct SelectMedia;
impl Callback for SelectMedia {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        assert_eq!(request.headers()["Sec-WebSocket-Protocol"], "media");
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("media"));
        Ok(response)
    }
}

async fn receive_request(peer: &mut tokio::net::TcpStream) -> Result<(), ControlError> {
    let mut request = Vec::with_capacity(4096);
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte).await.map_err(failure)?;
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") {
            return Ok(());
        }
        if request.len() >= 4096 {
            return Err(ControlError::Capacity);
        }
    }
}

#[tokio::test]
async fn a_failed_upgrade_retries_and_returns_the_successful_owned_connection(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?
        .with_websocket_configuration("", &format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{address}/media\nprotocols=media\nconnection_timeout=500\nreconnect_interval=100\nreconnect_attempts=2\n")).map_err(failure)?;
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let server = async {
            let (mut refused, _) = listener.accept().await.map_err(failure)?;
            receive_request(&mut refused).await?;
            let failed = tokio::time::Instant::now();
            refused.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.map_err(failure)?;
            drop(refused);
            let (accepted, _) = listener.accept().await.map_err(failure)?;
            assert!(failed.elapsed() >= Duration::from_millis(100));
            tokio_tungstenite::accept_hdr_async(accepted, SelectMedia).await.map_err(failure)
        };
        let (client, server) = tokio::join!(connect(&config.websocket.clients[0]), server);
        let mut client = client?;
        let mut server = server?;
        client.send(Message::Binary(vec![0xff; 160].into())).await.map_err(failure)?;
        assert!(matches!(server.next().await.ok_or(ControlError::Capacity)?.map_err(failure)?, Message::Binary(bytes) if bytes.as_ref() == [0xff; 160]));
        Ok::<(), ControlError>(())
    }).await.map_err(failure)?;
    result
}

#[tokio::test]
async fn each_upgrade_timeout_closes_its_socket_before_the_next_bounded_attempt(
) -> Result<(), ControlError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(failure)?;
    let address = listener.local_addr().map_err(failure)?;
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(failure)?
        .with_websocket_configuration("", &format!("[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://{address}/media\nprotocols=media\nconnection_timeout=80\nreconnect_interval=50\nreconnect_attempts=2\n")).map_err(failure)?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let server = async {
            for _ in 0..2 {
                let (mut peer, _) = listener.accept().await.map_err(failure)?;
                receive_request(&mut peer).await?;
                let mut byte = [0];
                assert_eq!(peer.read(&mut byte).await.map_err(failure)?, 0);
            }
            Ok::<(), ControlError>(())
        };
        let started = tokio::time::Instant::now();
        let (client, server) = tokio::join!(connect(&config.websocket.clients[0]), server);
        assert!(matches!(client, Err(ControlError::Adapter(message)) if message == "WebSocket connection failed"));
        assert!(started.elapsed() >= Duration::from_millis(210));
        server
    }).await.map_err(failure)?
}
