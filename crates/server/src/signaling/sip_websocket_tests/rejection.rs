use super::{configuration, failure, receive, request, ControlError, Server};
use futures_util::{SinkExt, StreamExt};
use rsipstack::sip::SipMessage;
use rusterisk_test_support::Wait;
use tokio_tungstenite::tungstenite::{Error, Message};

#[tokio::test]
async fn signaling_upgrades_reject_untrusted_origins_and_missing_sip_subprotocol(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut config = configuration("ws", "")?;
    config.http.bind.set_port(0);
    let server = Server::bind(config, 4, directory.path().join("server")).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let server = tokio::spawn(server.run());
    let result = async {
        for (protocol, origin, expected) in [(Some("sip"), Some("https://untrusted.example"), 403), (None, Some("https://browser.example"), 400), (Some("other"), Some("https://browser.example"), 400)] {
            let result = tokio_tungstenite::connect_async(request(address, protocol, origin)?).await;
            assert!(matches!(result, Err(Error::Http(response)) if response.status().as_u16() == expected));
        }
        let mut duplicated = request(address, Some("sip"), Some("https://browser.example"))?;
        duplicated.headers_mut().append("Origin", "https://browser.example".parse().map_err(failure)?);
        assert!(matches!(tokio_tungstenite::connect_async(duplicated).await, Err(Error::Http(response)) if response.status().as_u16() == 403));
        let (mut socket, response) = tokio_tungstenite::connect_async(request(address, Some("other, sip"), None)?).await.map_err(failure)?;
        assert_eq!(response.headers().get("Sec-WebSocket-Protocol").ok_or(ControlError::Capacity)?, "sip");
        socket.close(None).await.map_err(failure)?;
        Ok(())
    }.await;
    controller.shutdown();
    server.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn fragmented_websocket_frames_preserve_one_sip_message_and_joined_messages_close(
) -> Result<(), ControlError> {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        coding::{Data, OpCode},
        Frame,
    };
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut config = configuration("ws", "")?;
    config.http.bind.set_port(0);
    let server = Server::bind(config, 4, directory.path().join("server")).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let server = tokio::spawn(server.run());
    let result = async {
        let (mut socket, _) = tokio_tungstenite::connect_async(request(address, Some("sip"), None)?).await.map_err(failure)?;
        let options = b"OPTIONS sip:localhost SIP/2.0\r\nVia: SIP/2.0/WS browser.invalid;branch=z9hG4bK-fragment\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:server@localhost>\r\nCall-ID: websocket-options\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n";
        socket.send(Message::Frame(Frame::message(options[..17].to_vec(), OpCode::Data(Data::Text), false))).await.map_err(failure)?;
        assert!(Wait::Quiet.bounded(socket.next()).await.is_err());
        socket.send(Message::Frame(Frame::message(options[17..].to_vec(), OpCode::Data(Data::Continue), true))).await.map_err(failure)?;
        let SipMessage::Response(response) = receive(&mut socket).await? else { return Err(ControlError::Unsupported); };
        assert_eq!(response.status_code.code(), 200);
        let mut joined = options.to_vec();
        joined.extend_from_slice(options);
        socket.send(Message::Binary(joined.into())).await.map_err(failure)?;
        let closed = Wait::Ready.bounded(socket.next()).await.map_err(failure)?;
        assert!(matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_)) | None));
        Ok(())
    }.await;
    controller.shutdown();
    server.await.map_err(failure)??;
    result
}

#[tokio::test]
async fn upgrade_capacity_includes_active_sip_websockets_and_resumes_after_close(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut config = configuration("ws", "")?;
    config.http.bind.set_port(0);
    let server = Server::bind(config, 4, directory.path().join("server")).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let server = tokio::spawn(server.run());
    let result = async {
        let mut sockets = Vec::with_capacity(64);
        for serial in 0..64 {
            let (mut socket, _) = tokio_tungstenite::connect_async(request(address, Some("sip"), None)?).await.map_err(failure)?;
            let options = format!("OPTIONS sip:localhost SIP/2.0\r\nVia: SIP/2.0/WS browser.invalid;branch=z9hG4bK-capacity-{serial}\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:server@localhost>\r\nCall-ID: capacity-{serial}\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
            socket.send(Message::Text(options.into())).await.map_err(failure)?;
            let SipMessage::Response(response) = receive(&mut socket).await? else { return Err(ControlError::Unsupported); };
            assert_eq!(response.status_code.code(), 200);
            sockets.push(socket);
        }
        assert!(matches!(tokio_tungstenite::connect_async(request(address, Some("sip"), None)?).await, Err(Error::Http(response)) if response.status().as_u16() == 503));
        let mut released = sockets.pop().ok_or(ControlError::Capacity)?;
        released.close(None).await.map_err(failure)?;
        drop(released);
        let mut replacement = Wait::Ready.bounded(async {
            loop {
                match tokio_tungstenite::connect_async(request(address, Some("sip"), None)?).await {
                    Ok((socket, _)) => return Ok::<_, ControlError>(socket),
                    Err(Error::Http(response)) if response.status().as_u16() == 503 => {},
                    Err(error) => return Err(failure(error)),
                }
            }
        }).await.map_err(failure)??;
        replacement.close(None).await.map_err(failure)?;
        drop(sockets);
        Ok(())
    }.await;
    controller.shutdown();
    server.await.map_err(failure)??;
    result
}
