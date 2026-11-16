use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_config::{Config, Protocol, RegistrarUri};
use rusterisk_server::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
#[path = "outbound_registration/authentication.rs"]
mod authentication;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn finish(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(timeout)),
                Err(error) => Err(failure(error)),
                Ok(Err(error)) => Err(error),
                Ok(Ok(())) => Err(failure(timeout)),
            }
        }
    }
}

fn outcome(
    primary: Result<(), ControlError>,
    signaling: Result<(), ControlError>,
    runner: Result<(), ControlError>,
) -> Result<(), ControlError> {
    let mut failures = String::new();
    for (name, result) in [
        ("registration", primary),
        ("signaling cleanup", signaling),
        ("runner cleanup", runner),
    ] {
        if let Err(error) = result {
            if !failures.is_empty() {
                failures.push_str("; ");
            }
            failures.push_str(name);
            failures.push_str(": ");
            failures.push_str(&error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ControlError::Adapter(failures))
    }
}

fn settings(peer: SocketAddr) -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[native]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[account]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri=SIP:127.0.0.1:0{}\\;transport=UDP\\;extension=%61b\nclient_uri=sip:%61ccount@provider.example\ncontact_user=line+1\nmax_random_initial_delay=0\n", peer.port()),
        "",
        "",
    ).map_err(failure)
}

#[tokio::test]
async fn configured_udp_registration_advertises_the_owned_signaling_address(
) -> Result<(), ControlError> {
    let peer = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = peer.local_addr().map_err(failure)?;
    let config = settings(remote)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let stopped = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let signaling = tokio::spawn(signaling.run(stopped));
    let result: Result<(), ControlError> = async {
        let mut bytes = [0; 16385];
        if tokio::time::timeout(Duration::from_millis(500), peer.recv_from(&mut bytes))
            .await
            .is_ok()
        {
            return Err(ControlError::Invalid("Registration preceded initial delay"));
        }
        let (size, address) =
            tokio::time::timeout(Duration::from_secs(3), peer.recv_from(&mut bytes))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        if address != local {
            return Err(ControlError::Invalid(
                "Registration used a different listener",
            ));
        }
        let wire = std::str::from_utf8(&bytes[..size]).map_err(failure)?;
        if !wire.starts_with(&format!(
            "REGISTER SIP:127.0.0.1:0{};transport=UDP;extension=%61b SIP/2.0\r\n",
            remote.port()
        )) {
            return Err(ControlError::Invalid(
                "Registration changed Request-URI octets",
            ));
        }
        let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected registration request"));
        };
        if request.method != Method::Register {
            return Err(ControlError::Invalid("Expected REGISTER method"));
        }
        if request.contact_header().map_err(failure)?.value()
            != format!("<sip:line+1@{local};transport=udp>")
        {
            return Err(ControlError::Invalid("Registration changed local Contact"));
        }
        if request.to_header().map_err(failure)?.value() != "<sip:%61ccount@provider.example>" {
            return Err(ControlError::Invalid(
                "Registration changed address of record",
            ));
        }
        if !request
            .headers
            .iter()
            .any(|header| header.name() == "Expires" && header.value() == "3600")
        {
            return Err(ControlError::Invalid(
                "Registration changed requested lifetime",
            ));
        }
        if !request.body.is_empty() {
            return Err(ControlError::Invalid("Registration had an unexpected body"));
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    let signaling_result = finish(signaling).await;
    let runner_result = finish(runner).await;
    outcome(result, signaling_result, runner_result)?;
    let _rebound = UdpSocket::bind(local).await.map_err(failure)?;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), peer.recv_from(&mut [0; 16385]))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn tls_registration_without_a_connected_adapter_is_identifiably_unsupported(
) -> Result<(), ControlError> {
    let mut config = settings("127.0.0.1:5070".parse().map_err(failure)?)?;
    config
        .transports
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .protocol = Protocol::Tls;
    config
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?
        .server_uri =
        RegistrarUri::parse(Protocol::Tls, "sips:127.0.0.1:5071;transport=tcp").map_err(failure)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    assert!(matches!(
        Signaling::bind(config, controller, effects, media, notices).await,
        Err(ControlError::Unsupported)
    ));
    Ok(())
}

#[tokio::test]
async fn settings_without_registrations_keep_the_listener_silent_and_release_it_on_shutdown(
) -> Result<(), ControlError> {
    let peer = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let mut config = settings(peer.local_addr().map_err(failure)?)?;
    config.registrations.clear();
    let (controller, runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let stopped = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let signaling = tokio::spawn(signaling.run(stopped));
    let result =
        tokio::time::timeout(Duration::from_millis(100), peer.recv_from(&mut [0; 16385])).await;
    controller.shutdown();
    let signaling_result = finish(signaling).await;
    let runner_result = finish(runner).await;
    let primary = if result.is_err() {
        Ok(())
    } else {
        Err(ControlError::Invalid(
            "Unconfigured registration sent a packet",
        ))
    };
    outcome(primary, signaling_result, runner_result)?;
    let _rebound = UdpSocket::bind(local).await.map_err(failure)?;
    Ok(())
}
