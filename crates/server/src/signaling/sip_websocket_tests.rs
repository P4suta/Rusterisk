use crate::{
    encryption::TlsClientContext, runtime::Server, Action, ControlError, Controller, Credentials,
};
use futures_util::{SinkExt, StreamExt};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_api::{Channel, ChannelId, ChannelState, Endpoint, EndpointState};
use rusterisk_config::{Codec, Config};
use rusterisk_sip::Session;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    WebSocketStream,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

mod rejection;

fn request(
    address: SocketAddr,
    protocol: Option<&str>,
    origin: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, ControlError> {
    let mut request = format!("ws://{address}/ws")
        .into_client_request()
        .map_err(failure)?;
    if let Some(protocol) = protocol {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", protocol.parse().map_err(failure)?);
    }
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().map_err(failure)?);
    }
    Ok(request)
}

fn configuration(protocol: &str, encryption: &str) -> Result<Config, ControlError> {
    Config::parse(&format!("[general]\nbindaddr=127.0.0.1\n{encryption}"), "[general]\nallowed_origins=https://browser.example\n[app]\ntype=user\npassword=test-password\n", &format!("[browser]\ntype=transport\nprotocol={protocol}\nbind=0.0.0.0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[aor]\ntype=aor\n[phone]\ntype=endpoint\ntransport=browser\nauth=auth\naors=aor\nallow=ulaw\n"), "", "").map_err(failure)
}

async fn receive<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Result<SipMessage, ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(bytes))) => {
                    return SipMessage::try_from(bytes.as_bytes()).map_err(failure);
                }
                Some(Ok(Message::Binary(bytes))) => {
                    return SipMessage::try_from(bytes.as_ref()).map_err(failure);
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_) | Message::Frame(_))) | None => {
                    return Err(ControlError::Invalid("Signaling connection closed"));
                }
                Some(Err(error)) => return Err(failure(error)),
            }
        }
    })
    .await
    .map_err(failure)?
}

fn registration(token: &str, sequence: u32, authorization: &str) -> String {
    format!(
        "REGISTER sip:localhost SIP/2.0\r\nVia: SIP/2.0/{token} browser.invalid;branch=z9hG4bK-register-{sequence};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: browser-registration\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:phone@browser.invalid;transport=ws>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}\r\n"
    )
}

async fn registered<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    token: &str,
) -> Result<(), ControlError> {
    socket
        .send(Message::Text(registration(token, 1, "").into()))
        .await
        .map_err(failure)?;
    let SipMessage::Response(challenge) = receive(socket).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(challenge.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Capacity)?
            .value(),
    )
    .map_err(failure)?;
    let uri = Uri::try_from("sip:localhost").map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "browser".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "test-password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!(
        "Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"browser\", nc=00000001\r\n",
        challenge.realm, challenge.nonce
    );
    socket
        .send(Message::Binary(
            registration(token, 2, &authorization).into_bytes().into(),
        ))
        .await
        .map_err(failure)?;
    let SipMessage::Response(response) = receive(socket).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(response.status_code.code(), 200);
    Ok(())
}

async fn endpoint_state(
    controller: &Controller,
    expected: EndpointState,
) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = controller
                .execute(
                    credentials()?,
                    None,
                    Action::GetEndpoint {
                        technology: "PJSIP".into(),
                        resource: "phone".into(),
                    },
                )
                .await?;
            let endpoint: Endpoint =
                serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
            if endpoint.state == Some(expected) {
                return Ok::<(), ControlError>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(failure)?
}

async fn exercise<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
    token: &str,
    controller: &Controller,
) -> Result<(), ControlError> {
    registered(&mut socket, token).await?;
    endpoint_state(controller, EndpointState::Online).await?;
    let subscription = controller
        .subscribe(credentials()?, None, "app".into())
        .await?;
    let id = ChannelId::try_from("browser-call".to_owned())?;
    controller
        .execute(
            credentials()?,
            None,
            Action::Originate {
                id: id.clone(),
                endpoint: "PJSIP/phone".into(),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 30,
            },
        )
        .await?;
    let SipMessage::Request(invite) = receive(&mut socket).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(invite.method, Method::Invite);
    assert!(invite
        .top_via_header()
        .map_err(failure)?
        .value()
        .starts_with(&format!("SIP/2.0/{token} ")));
    let body = Session::parse(
        std::str::from_utf8(&invite.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?
    .answer("127.0.0.1:19000".parse().map_err(failure)?, 1)
    .map_err(failure)?;
    let response = format!(
        "SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=browser\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@browser.invalid;transport=ws>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        invite.top_via_header().map_err(failure)?.value(),
        invite.from_header().map_err(failure)?.value(),
        invite.to_header().map_err(failure)?.value(),
        invite.call_id_header().map_err(failure)?.value(),
        invite.cseq_header().map_err(failure)?.value(),
        body.len()
    );
    socket
        .send(Message::Text(response.into()))
        .await
        .map_err(failure)?;
    let SipMessage::Request(ack) = receive(&mut socket).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(ack.method, Method::Ack);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let channel = controller
                .execute(credentials()?, None, Action::GetChannel(id.clone()))
                .await?;
            let channel: Channel =
                serde_json::from_value(channel.body.ok_or(ControlError::Capacity)?)?;
            if channel.state == ChannelState::Up {
                return Ok::<(), ControlError>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(failure)??;
    controller
        .execute(credentials()?, None, Action::Hangup(id))
        .await?;
    let SipMessage::Request(bye) = receive(&mut socket).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(bye.method, Method::Bye);
    socket.close(None).await.map_err(failure)?;
    drop(socket);
    endpoint_state(controller, EndpointState::Offline).await?;
    drop(subscription);
    Ok(())
}

#[tokio::test]
async fn http_signaling_websockets_negotiate_sip_and_reuse_authenticated_ws_and_wss_flows(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let identity = rcgen::generate_simple_self_signed(["localhost".into()]).map_err(failure)?;
    let certificate = directory.path().join("certificate.pem");
    let private_key = directory.path().join("private-key.pem");
    std::fs::write(&certificate, identity.cert.pem()).map_err(failure)?;
    std::fs::write(&private_key, identity.signing_key.serialize_pem()).map_err(failure)?;
    for (protocol, token) in [("ws", "WS"), ("wss", "WSS")] {
        let encryption = if protocol == "wss" {
            format!(
                "tlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile={}\ntlsprivatekey={}\n",
                certificate.display(),
                private_key.display()
            )
        } else {
            String::new()
        };
        let mut config = configuration(protocol, &encryption)?;
        config.http.bind.set_port(0);
        let server = Server::bind(config, 4, directory.path().join(protocol)).await?;
        let address = server.local_addr()?;
        assert_eq!(server.sip_listeners()?[0].1, address);
        let controller = server.controller();
        let server = tokio::spawn(server.run());
        let result = async {
            let mut request = format!("{protocol}://localhost:{}/ws", address.port())
                .into_client_request()
                .map_err(failure)?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "sip".parse().map_err(failure)?);
            request.headers_mut().insert(
                "Origin",
                "https://browser.example".parse().map_err(failure)?,
            );
            let stream = TcpStream::connect(address).await.map_err(failure)?;
            match protocol {
                "ws" => {
                    let (socket, response) = tokio_tungstenite::client_async(request, stream)
                        .await
                        .map_err(failure)?;
                    assert_eq!(
                        response
                            .headers()
                            .get("Sec-WebSocket-Protocol")
                            .ok_or(ControlError::Capacity)?,
                        "sip"
                    );
                    exercise(socket, token, &controller).await
                }
                "wss" => {
                    let client =
                        TlsClientContext::load(Some(certificate.to_string_lossy().as_ref()))?;
                    let stream = client
                        .connect(stream, ServerName::try_from("localhost").map_err(failure)?)
                        .await?;
                    let (socket, response) = tokio_tungstenite::client_async(request, stream)
                        .await
                        .map_err(failure)?;
                    assert_eq!(
                        response
                            .headers()
                            .get("Sec-WebSocket-Protocol")
                            .ok_or(ControlError::Capacity)?,
                        "sip"
                    );
                    exercise(socket, token, &controller).await
                }
                _ => Err(ControlError::Unsupported),
            }
        }
        .await;
        controller.shutdown();
        server.await.map_err(failure)??;
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(failure)?;
        assert_eq!(listener.local_addr().map_err(failure)?, address);
        result?;
    }
    Ok(())
}
