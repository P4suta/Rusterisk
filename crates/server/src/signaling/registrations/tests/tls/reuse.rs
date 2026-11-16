use super::{configuration, failure, finish, Identity};
use crate::{
    encryption::TlsContext,
    media::MediaHub,
    signaling::{registrations::tests::reuse, Signaling},
    ControlError, Controller,
};
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn initial_tls_registrations_reuse_the_verified_profile_before_connecting(
) -> Result<(), ControlError> {
    let identity = Identity::new("127.0.0.1")?;
    let context = TlsContext::load(&identity.files).await?;
    let registrar = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let (mut config, uri) = configuration(
        registrar.local_addr().map_err(failure)?,
        &identity,
        &identity,
    )?;
    reuse::add_registration(&mut config)?;
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
        let (socket, _address) = tokio::time::timeout(Duration::from_secs(3), registrar.accept())
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let mut peer = context.accept(socket).await?;
        reuse::requests(&mut peer, &registrar, local, &uri, true).await
    }
    .await;
    finish(controller, task, primary).await?;
    let _rebound = TcpListener::bind(local).await.map_err(failure)?;
    Ok(())
}
