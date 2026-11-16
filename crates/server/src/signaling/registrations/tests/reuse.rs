use super::stream::{configuration, receive, reply, Packet};
use crate::{
    media::MediaHub,
    signaling::{cleanup, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::HeadersExt;
use rusterisk_config::{Config, RegistrationAddress};
use rusterisk_sip::SipStreamDecoder;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
mod fallback;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

pub(super) fn add_registration(config: &mut Config) -> Result<(), ControlError> {
    let mut second = config
        .registrations
        .first()
        .ok_or(ControlError::Capacity)?
        .clone();
    second.name = "second".into();
    second.contact_user = "second".into();
    let scheme = if second.client_uri.is_secure() {
        "sips"
    } else {
        "sip"
    };
    second.client_uri =
        RegistrationAddress::parse(&format!("{scheme}:second@provider.test")).map_err(failure)?;
    config.registrations.push(second);
    Ok(())
}

async fn next<S: AsyncRead + Unpin>(
    peer: &mut S,
    decoder: &mut SipStreamDecoder,
    registrar: &TcpListener,
) -> Result<Packet, ControlError> {
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            biased;
            extra = registrar.accept() => {
                let (_socket, _peer) = extra.map_err(failure)?;
                Err(ControlError::Invalid("Registration attempted another connection before checking reuse"))
            }
            packet = receive(peer, decoder) => packet,
        }
    }).await.map_err(failure)?
}

pub(super) async fn requests<S: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut S,
    registrar: &TcpListener,
    local: SocketAddr,
    uri: &str,
    secure: bool,
) -> Result<(), ControlError> {
    let mut decoder = SipStreamDecoder::new();
    let initial = next(peer, &mut decoder, registrar).await?;
    if initial.uri != uri || initial.request.cseq_header().map_err(failure)?.value() != "1 REGISTER"
    {
        return Err(ControlError::Invalid(
            "Initial registration lost its configured identity",
        ));
    }
    let mut byte = [0; 1];
    let (connection, packet) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(50), registrar.accept()),
        tokio::time::timeout(Duration::from_millis(50), peer.read(&mut byte)),
    );
    match connection {
        Ok(Ok(_)) => {
            return Err(ControlError::Invalid(
                "Registration connected before the preceding final response",
            ))
        }
        Ok(Err(error)) => return Err(failure(error)),
        Err(_) => {}
    }
    match packet {
        Ok(Ok(0)) => {
            return Err(ControlError::Invalid(
                "Registrar flow closed before its first grant",
            ))
        }
        Ok(Ok(_)) => {
            return Err(ControlError::Invalid(
                "Registration bypassed the same-registrar final-response barrier",
            ))
        }
        Ok(Err(error)) => return Err(failure(error)),
        Err(_) => {}
    }
    let contact = initial.request.contact_header().map_err(failure)?.value();
    peer.write_all(
        reply(
            &initial.request,
            200,
            &format!("Contact: {contact};expires=60\r\nExpires: 60\r\n"),
        )?
        .as_bytes(),
    )
    .await
    .map_err(failure)?;
    let second = next(peer, &mut decoder, registrar).await?;
    let scheme = if secure { "sips" } else { "sip" };
    let contact = format!("<{scheme}:second@{local};transport=tcp>");
    if second.uri != uri
        || second.request.cseq_header().map_err(failure)?.value() != "2 REGISTER"
        || second.request.call_id_header().map_err(failure)?.value()
            != initial.request.call_id_header().map_err(failure)?.value()
        || second.request.contact_header().map_err(failure)?.value() != contact
        || !second
            .request
            .from_header()
            .map_err(failure)?
            .value()
            .contains("second@provider.test")
    {
        return Err(ControlError::Invalid(
            "Reused registration changed the client or request sequence",
        ));
    }
    peer.write_all(
        reply(
            &second.request,
            200,
            &format!("Contact: {contact};expires=60\r\nExpires: 60\r\n"),
        )?
        .as_bytes(),
    )
    .await
    .map_err(failure)?;
    match tokio::time::timeout(Duration::from_millis(100), registrar.accept()).await {
        Ok(Ok(_)) => {
            return Err(ControlError::Invalid(
                "Registration opened an unnecessary connection",
            ))
        }
        Ok(Err(error)) => return Err(failure(error)),
        Err(_) => {}
    }
    Ok(())
}

async fn finish(mut task: JoinHandle<Result<(), ControlError>>) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(timeout) => {
            task.abort();
            let joined = match task.await {
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
                Ok(result) => result,
            };
            cleanup::combine(Err(failure(timeout)), joined)
        }
    }
}

#[tokio::test]
async fn ready_registrations_keep_the_same_registrar_setup_barrier() -> Result<(), ControlError> {
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let uri = format!(
        "sip:{};transport=tcp",
        registrar.local_addr().map_err(failure)?
    );
    let mut config = configuration(&uri)?;
    add_registration(&mut config)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
    let primary: Result<(), ControlError> = async {
        signaling.registration_tick(1000).await?;
        let completed =
            tokio::time::timeout(Duration::from_secs(3), signaling.registrations.completed())
                .await
                .map_err(failure)??
                .ok_or(ControlError::Capacity)?;
        signaling.registration_completed(completed, 1000)?;
        let crate::signaling::transport::Input::Prepared(prepared) =
            tokio::time::timeout(Duration::from_secs(3), signaling.network.next())
                .await
                .map_err(failure)??
        else {
            return Err(ControlError::Invalid(
                "Expected initial registration preparation",
            ));
        };
        let (mut peer, _address) = tokio::time::timeout(Duration::from_secs(3), registrar.accept())
            .await
            .map_err(failure)?
            .map_err(failure)?;
        signaling.registration_prepared(prepared, 1000)?;
        signaling.registration_tick(1000).await?;
        if !matches!(
            signaling.registrations.clients[0].state,
            super::State::Trying { .. }
        ) || !matches!(
            signaling.registrations.clients[1].state,
            super::State::Waiting { .. }
        ) || signaling.registrations.setup_count() != 0
        {
            return Err(ControlError::Invalid(
                "Ready registration yielded ownership before transmitting its request",
            ));
        }
        let initial = receive(&mut peer, &mut SipStreamDecoder::new()).await?;
        if initial.request.cseq_header().map_err(failure)?.value() != "1 REGISTER" {
            return Err(ControlError::Invalid(
                "Prepared registration lost request sequencing",
            ));
        }
        Ok(())
    }
    .await;
    let result = cleanup::combine(primary, signaling.finish_registrations().await);
    cleanup::combine(result, signaling.network.close().await)
}

#[tokio::test]
async fn initial_tcp_registrations_reuse_an_established_flow_before_connecting(
) -> Result<(), ControlError> {
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let uri = format!(
        "sip:{};transport=tcp",
        registrar.local_addr().map_err(failure)?
    );
    let mut config = configuration(&uri)?;
    add_registration(&mut config)?;
    let (controller, _runner, effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let task = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let primary: Result<(), ControlError> = async {
        let (mut peer, _address) = tokio::time::timeout(Duration::from_secs(3), registrar.accept())
            .await
            .map_err(failure)?
            .map_err(failure)?;
        requests(&mut peer, &registrar, local, &uri, false).await
    }
    .await;
    controller.shutdown();
    cleanup::combine(primary, finish(task).await)?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}
