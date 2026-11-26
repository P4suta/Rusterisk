use super::{
    candidates, cleanup, config, dns, failure, finish, receive, require, respond, silent,
    successor, ControlError, Controller, HeadersExt, MediaHub, Packet, RegistrarUri, Signaling,
};
use crate::signaling::resolver::ResolutionCache;
use rusterisk_config::Protocol;
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

async fn resolved(
    signaling: &mut Signaling,
    dns: &UdpSocket,
    primary: SocketAddr,
    backup: SocketAddr,
    now: u64,
) -> Result<(), ControlError> {
    let (completion, answered) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(1), signaling.registrations.completed()),
        candidates(dns, primary, backup),
    );
    let completion = completion
        .map_err(failure)
        .and_then(|result| result)
        .and_then(|result| {
            result.ok_or(ControlError::Invalid(
                "Persistent peer wait resolution job disappeared",
            ))
        });
    let completion = match (completion, answered) {
        (Ok(completion), Ok(())) => completion,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(answered)) => {
            return Err(failure(format!(
                "{primary}; persistent wait DNS failed: {answered}"
            )))
        }
    };
    signaling.registration_completed(completion, now)
}

async fn backup_without_primary(
    primary: &UdpSocket,
    backup: &UdpSocket,
) -> Result<Packet, ControlError> {
    let (packet, quiet) = tokio::join!(
        receive(backup, "Backup REGISTER before the primary retry deadline"),
        silent(primary),
    );
    match (packet, quiet) {
        (Ok(packet), Ok(())) => Ok(packet),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(quiet)) => Err(failure(format!(
            "{primary}; primary silence failed: {quiet}"
        ))),
    }
}

#[tokio::test]
async fn a_primary_retry_deadline_survives_backup_success_failure_and_fresh_resolution(
) -> Result<(), ControlError> {
    let primary = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let backup = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let dns_socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let primary_address = primary.local_addr().map_err(failure)?;
    let backup_address = backup.local_addr().map_err(failure)?;
    let mut settings = config(&["provider.test"], 0)?;
    let uri = "SIP:provider.test;transport=UDP;extension=%61b";
    let registration = settings
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?;
    registration.server_uri = RegistrarUri::parse(Protocol::Udp, uri).map_err(failure)?;
    registration.expiration = 11;
    registration.retry_interval = 1;
    registration.max_retries = 1;
    let (controller, runner, _effects) = Controller::new(settings.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(settings, controller, runner, media, notices).await?;
    signaling.resolver = Ok(dns::resolver(&dns_socket)?);
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let result = async {
        signaling.registration_tick(1000).await?;
        resolved(
            &mut signaling,
            &dns_socket,
            primary_address,
            backup_address,
            1000,
        )
        .await?;
        signaling.registration_tick(1000).await?;
        let initial = receive(&primary, "Initial primary REGISTER for persistent wait").await?;
        respond(
            &mut signaling,
            &primary,
            &initial,
            503,
            "Retry-After: 10\r\n",
            1100,
        )
        .await?;
        signaling.registration_tick(1200).await?;
        let alternate = backup_without_primary(&primary, &backup).await?;
        successor(&initial, &[&initial], &alternate, 2)?;
        let contact = alternate.request.contact_header().map_err(failure)?.value();
        respond(
            &mut signaling,
            &backup,
            &alternate,
            200,
            &format!("Contact: {contact};expires=11\r\n"),
            1300,
        )
        .await?;
        signaling.registration_tick(2300).await?;
        let refresh = receive(&backup, "Backup refresh after successful registration").await?;
        successor(&initial, &[&initial, &alternate], &refresh, 3)?;
        respond(
            &mut signaling,
            &backup,
            &refresh,
            500,
            "Retry-After: 1\r\n",
            2400,
        )
        .await?;
        signaling.resolution_cache = ResolutionCache::default();
        signaling.resolve_registration(0)?;
        resolved(
            &mut signaling,
            &dns_socket,
            primary_address,
            backup_address,
            3400,
        )
        .await?;
        signaling.registration_tick(3400).await?;
        let retried = backup_without_primary(&primary, &backup).await?;
        require(
            retried.uri == uri
                && retried.peer == local
                && retried.request.cseq_header().map_err(failure)?.value() == "4 REGISTER",
            "Fresh resolution changed the starting registration target or sequence",
        )?;
        successor(&initial, &[&initial, &alternate, &refresh], &retried, 4)?;
        respond(
            &mut signaling,
            &backup,
            &retried,
            500,
            "Retry-After: 20\r\n",
            3500,
        )
        .await?;
        signaling.registration_tick(11_099).await?;
        let (primary_quiet, backup_quiet) = tokio::join!(silent(&primary), silent(&backup));
        cleanup::combine(primary_quiet, backup_quiet)?;
        signaling.resolution_cache = ResolutionCache::default();
        signaling.resolve_registration(0)?;
        resolved(
            &mut signaling,
            &dns_socket,
            primary_address,
            backup_address,
            11_100,
        )
        .await?;
        signaling.registration_tick(11_100).await?;
        let available = receive(&primary, "Primary REGISTER at its exact retry deadline").await?;
        require(
            available.uri == uri
                && available.peer == local
                && available.request.cseq_header().map_err(failure)?.value() == "5 REGISTER",
            "An expired primary retry deadline did not restore the original target",
        )?;
        successor(
            &initial,
            &[&initial, &alternate, &refresh, &retried],
            &available,
            5,
        )?;
        require(
            signaling.registrations.setup_count() == 0,
            "Persistent retry wait retained a resolution job",
        )
    }
    .await;
    cleanup::combine(result, finish(signaling, local).await)
}
