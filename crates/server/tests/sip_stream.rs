use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_api::{Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub, signaling::Signaling, Action, ControlError, Controller, Credentials,
};
use rusterisk_sip::{SipStreamDecoder, StreamItem};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=phone\ncontext=internal\nallow=ulaw\n", "", "[internal]\nexten => 100,1,Stasis(calls)\n").map_err(failure)
}

async fn receive(
    peer: &mut TcpStream,
    decoder: &mut SipStreamDecoder,
) -> Result<SipMessage, ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut byte = [0; 1];
        loop {
            if peer.read(&mut byte).await.map_err(failure)? == 0 {
                return Err(ControlError::Adapter("Signaling peer closed".into()));
            }
            let (_, item) = decoder.push(&byte).map_err(failure)?;
            match item {
                Some(StreamItem::Message(bytes)) => {
                    return SipMessage::try_from(bytes.as_slice()).map_err(failure)
                }
                Some(StreamItem::Crlf) | None => {}
            }
        }
    })
    .await
    .map_err(failure)?
}

fn register(local: SocketAddr, target: SocketAddr, serial: u32, authorization: &str) -> String {
    format!("REGISTER sip:{target} SIP/2.0\r\nVia: SIP/2.0/TCP {local};branch=z9hG4bK-register-{serial};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: registration\r\nCSeq: {serial} REGISTER\r\nContact: <sip:phone@{local};transport=tcp>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
}

async fn registered(
    peer: &mut TcpStream,
    target: SocketAddr,
    decoder: &mut SipStreamDecoder,
) -> Result<(), ControlError> {
    let local = peer.local_addr().map_err(failure)?;
    let initial = register(local, target, 1, "");
    peer.write_all(&initial.as_bytes()[..17])
        .await
        .map_err(failure)?;
    let mut byte = [0; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(30), peer.read(&mut byte))
            .await
            .is_err()
    );
    peer.write_all(&initial.as_bytes()[17..])
        .await
        .map_err(failure)?;
    let SipMessage::Response(challenge) = receive(peer, decoder).await? else {
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
    let uri = Uri::try_from(format!("sip:{target}")).map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "stream-client".into(),
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
    let auth = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"stream-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    peer.write_all(register(local, target, 2, &auth).as_bytes())
        .await
        .map_err(failure)?;
    let SipMessage::Response(response) = receive(peer, decoder).await? else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(response.status_code.code(), 200);
    Ok(())
}

async fn state(controller: &Controller, expected: EndpointState) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = controller
                .execute(
                    Credentials::new("app".into(), "test-password".into())?,
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

#[tokio::test]
async fn tcp_registration_uses_fragmented_messages_and_disconnect_releases_presence_and_listener(
) -> Result<(), ControlError> {
    let config = configuration()?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let address = signaling.listeners()?[0].1;
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        let mut peer = TcpStream::connect(address).await.map_err(failure)?;
        let mut decoder = SipStreamDecoder::new();
        registered(&mut peer, address, &mut decoder).await?;
        state(&controller, EndpointState::Online).await?;
        peer.write_all(b"\r\n\r\n").await.map_err(failure)?;
        let mut pong = [0; 2];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut pong))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert_eq!(pong, *b"\r\n");
        drop(peer);
        state(&controller, EndpointState::Offline).await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(failure)?;
    assert_eq!(listener.local_addr().map_err(failure)?, address);
    result
}
