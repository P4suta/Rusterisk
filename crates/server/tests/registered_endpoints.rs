use rsipstack::sip::SipMessage;
use rusterisk_api::{Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_server::{
    media::MediaHub, signaling::Signaling, Action, ControlError, Controller, Credentials,
};
use rusterisk_test_support::Wait;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn response(socket: &UdpSocket) -> Result<rsipstack::sip::Response, ControlError> {
    let mut bytes = [0; 16_385];
    let (size, _) = Wait::Ready
        .bounded(socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected a registration response")),
    }
}

#[path = "sip_call/registration.rs"]
mod registration;

async fn register(
    socket: &UdpSocket,
    target: SocketAddr,
    serial: u32,
    expires: u32,
) -> Result<usize, ControlError> {
    registration::register(
        socket,
        target,
        socket.local_addr().map_err(failure)?,
        "phone",
        "test-password",
        serial,
        expires,
    )
    .await
}

async fn wait_state(controller: &Controller, expected: EndpointState) -> Result<(), ControlError> {
    Wait::Ready
        .bounded(async {
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
            }
        })
        .await
        .map_err(failure)?
}

#[tokio::test]
async fn actual_registration_removal_and_expiration_reach_endpoint_queries(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=2\nminimum_expiration=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let phone = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let second = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        wait_state(&controller, EndpointState::Offline).await?;
        assert_eq!(register(&phone, target, 1, 20).await?, 1);
        wait_state(&controller, EndpointState::Online).await?;
        assert_eq!(register(&second, target, 2, 20).await?, 2);
        wait_state(&controller, EndpointState::Online).await?;
        assert_eq!(register(&phone, target, 3, 0).await?, 1);
        wait_state(&controller, EndpointState::Online).await?;
        assert_eq!(register(&second, target, 4, 0).await?, 0);
        wait_state(&controller, EndpointState::Offline).await?;
        assert_eq!(register(&phone, target, 5, 1).await?, 1);
        wait_state(&controller, EndpointState::Online).await?;
        wait_state(&controller, EndpointState::Offline).await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
