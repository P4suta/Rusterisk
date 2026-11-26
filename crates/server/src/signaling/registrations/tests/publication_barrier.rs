use super::{config, ControlError};
use crate::{
    media::MediaHub,
    reload::{PublicationKey, PublicationPhase, PublicationPort},
    signaling::{cleanup, NativePreparedSignaling, Signaling},
    Controller,
};
use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_core::SettingsLifetimes;
use rusterisk_test_support::Wait;
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

async fn command(
    signaling: &mut Signaling,
    port: &PublicationPort<NativePreparedSignaling>,
    key: PublicationKey,
    phase: PublicationPhase,
    now: u64,
) -> Result<(), ControlError> {
    let acknowledgement = port.command(key, phase)?;
    let command = signaling
        .publication
        .as_mut()
        .ok_or(ControlError::Unsupported)?
        .receiver
        .commands
        .try_recv()
        .map_err(failure)?;
    signaling.native_publication(command, now)?;
    acknowledgement.receive().await
}

async fn finish(
    mut signaling: Signaling,
    registrar: UdpSocket,
    primary: Result<(), ControlError>,
) -> Result<(), ControlError> {
    let local = signaling.network.local_addr(0);
    let remote = registrar.local_addr().map_err(failure);
    let mut result = cleanup::combine(primary, signaling.finish_registrations().await);
    result = cleanup::combine(result, signaling.finish_native().await);
    result = cleanup::combine(result, signaling.network.close().await);
    drop(signaling);
    drop(registrar);
    for address in [local, remote] {
        let rebound = match address {
            Ok(address) => UdpSocket::bind(address)
                .await
                .map(|_socket| ())
                .map_err(failure),
            Err(error) => Err(error),
        };
        result = cleanup::combine(result, rebound);
    }
    result
}

#[tokio::test]
async fn published_registrations_wait_for_resume_before_their_first_transaction(
) -> Result<(), ControlError> {
    let registrar = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let remote = registrar.local_addr().map_err(failure)?;
    let config = config(&[&remote.to_string()], 0)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut lifetimes = SettingsLifetimes::new(92);
    let base = lifetimes.current();
    let candidate = lifetimes.prepare().map_err(|_| ControlError::Capacity)?;
    let key = PublicationKey::new(candidate, base);
    let generation = lifetimes
        .commit(candidate)
        .map_err(|_| ControlError::Capacity)?
        .generation();
    let (port, receiver) = PublicationPort::channel();
    let mut signaling = Signaling::bind(config.clone(), controller, runner, media, notices)
        .await?
        .with_publication(receiver, base);
    let primary = Wait::Ready
        .bounded(async {
            let local = signaling.network.local_addr(0)?;
            let prepared = signaling.native_preparation().prepare(&config).await?;
            let staged = port.stage(key, prepared)?;
            let handoff = signaling
                .publication
                .as_mut()
                .ok_or(ControlError::Unsupported)?
                .receiver
                .prepared
                .try_recv()
                .map_err(failure)?;
            signaling.stage_native_settings(handoff)?;
            staged.receive().await?;
            command(&mut signaling, &port, key, PublicationPhase::Quiesce, 100).await?;
            command(
                &mut signaling,
                &port,
                key,
                PublicationPhase::Publish(generation),
                200,
            )
            .await?;
            require(
                !signaling.admitting(),
                "Publication reopened native admission before Resume",
            )?;
            let identity = signaling.registrations.identity;
            let mut bytes = [0; 2048];
            for now in [1199, 1200, 5000] {
                signaling.registration_tick(now).await?;
                match Wait::Quiet.bounded(registrar.recv_from(&mut bytes)).await {
                    Ok(Ok(_)) => {
                        return Err(ControlError::Invalid(
                            "Published registration transmitted before Resume",
                        ));
                    }
                    Ok(Err(error)) => return Err(failure(error)),
                    Err(_) => {}
                }
                require(
                    signaling.registrations.identity == identity
                        && signaling.registrations.setup_count() == 0,
                    "Publication allocated registration work before Resume",
                )?;
            }
            command(
                &mut signaling,
                &port,
                key,
                PublicationPhase::Resume(generation),
                5000,
            )
            .await?;
            require(signaling.admitting(), "Resume kept native admission closed")?;
            signaling.registration_tick(5000).await?;
            let (length, peer) = registrar.recv_from(&mut bytes).await.map_err(failure)?;
            let SipMessage::Request(request) =
                SipMessage::try_from(&bytes[..length]).map_err(failure)?
            else {
                return Err(ControlError::Invalid("Resume did not send a REGISTER"));
            };
            require(
                peer == local
                    && request.method == Method::Register
                    && request.cseq_header().map_err(failure)?.value() == "1 REGISTER"
                    && request.authorization_header().is_none(),
                "Resume changed the unsent registration identity or starting route",
            )
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
    finish(signaling, registrar, primary).await
}
