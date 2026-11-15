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
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn response(socket: &UdpSocket) -> Result<rsipstack::sip::Response, ControlError> {
    let mut bytes = [0; 16_385];
    let (size, _) = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected a registration response")),
    }
}

async fn register(
    socket: &UdpSocket,
    target: SocketAddr,
    serial: u32,
    expires: u32,
) -> Result<(), ControlError> {
    let local = socket.local_addr().map_err(failure)?;
    let uri = Uri::try_from(format!("sip:{target}")).map_err(failure)?;
    let packet = |sequence: u32, authorization: &str| {
        format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-register-{serial}-{sequence};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: registration-{serial}\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:phone@{local}>\r\nExpires: {expires}\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
    };
    socket
        .send_to(packet(1, "").as_bytes(), target)
        .await
        .map_err(failure)?;
    let challenge = response(socket).await?;
    assert_eq!(challenge.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Capacity)?
            .value(),
    )
    .map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "registration-client".into(),
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
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"registration-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    socket
        .send_to(packet(2, &authorization).as_bytes(), target)
        .await
        .map_err(failure)?;
    assert_eq!(response(socket).await?.status_code.code(), 200);
    Ok(())
}

async fn wait_state(controller: &Controller, expected: EndpointState) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let outcome = controller
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
                serde_json::from_value(outcome.body.ok_or(ControlError::Capacity)?)?;
            if endpoint.state == Some(expected) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(failure)?
}

#[tokio::test]
async fn actual_registration_removal_and_expiration_reach_endpoint_queries(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[dynamic]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let target = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let phone = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        wait_state(&controller, EndpointState::Offline).await?;
        register(&phone, target, 1, 20).await?;
        wait_state(&controller, EndpointState::Online).await?;
        register(&phone, target, 2, 0).await?;
        wait_state(&controller, EndpointState::Offline).await?;
        register(&phone, target, 3, 1).await?;
        wait_state(&controller, EndpointState::Online).await?;
        wait_state(&controller, EndpointState::Offline).await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
