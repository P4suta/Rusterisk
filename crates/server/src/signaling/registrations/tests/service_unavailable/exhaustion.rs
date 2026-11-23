use super::{
    candidates, cleanup, config, dns, failure, finish, receive, require, respond, silent,
    successor, ControlError, Controller, DestinationResolver, HeadersExt, MediaHub, Protocol,
    RegistrarUri, Signaling, StaticTarget,
};
use crate::signaling::registrations::State;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;

enum Scenario {
    ExhaustedBudget,
    PositiveInstruction,
}

async fn run(scenario: Scenario) -> Result<(), ControlError> {
    let primary = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let backup = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let dns_socket = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let primary_address = primary.local_addr().map_err(failure)?;
    let backup_address = backup.local_addr().map_err(failure)?;
    let resolver: DestinationResolver = dns::resolver(&dns_socket)?;
    let uri = "SIP:provider.test;transport=UDP;extension=%61b";
    let target = StaticTarget::from_uri(Protocol::Udp, uri).map_err(failure)?;
    let (resolution, answered) = tokio::join!(
        resolver.resolve(&target),
        candidates(&dns_socket, primary_address, backup_address),
    );
    let resolved = match (resolution, answered) {
        (Ok(resolved), Ok(())) => resolved,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(primary), Err(answered)) => {
            return Err(failure(format!(
                "{primary}; exhausted candidate DNS failed: {answered}"
            )))
        }
    };
    require(
        resolved.peers().eq([primary_address, backup_address]),
        "Exhausted candidate DNS order changed",
    )?;
    drop(dns_socket);
    let mut settings = config(&["provider.test"], 0)?;
    let registration = settings
        .registrations
        .first_mut()
        .ok_or(ControlError::Capacity)?;
    registration.server_uri = RegistrarUri::parse(Protocol::Udp, uri).map_err(failure)?;
    registration.retry_interval = 1;
    registration.max_retries = 0;
    let (controller, _runner, effects) = Controller::new(settings.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(settings, controller, effects, media, notices).await?;
    signaling.resolver = Ok(resolver);
    signaling
        .resolution_cache
        .insert(resolved, Instant::now())?;
    let local = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Capacity)?
        .1;
    let result = async {
        signaling.registration_tick(1000).await?;
        let completed =
            tokio::time::timeout(Duration::from_secs(1), signaling.registrations.completed())
                .await
                .map_err(failure)??
                .ok_or(ControlError::Invalid(
                    "Exhausted candidate resolution disappeared",
                ))?;
        signaling.registration_completed(completed, 1000)?;
        signaling.registration_tick(1000).await?;
        let initial = receive(&primary, "Initial REGISTER for candidate exhaustion").await?;
        require(
            initial.peer == local && initial.uri == uri,
            "Exhausted candidate initial route or URI changed",
        )?;
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
        let alternate = receive(&backup, "Backup REGISTER before candidate exhaustion").await?;
        successor(&initial, &[&initial], &alternate, 2)?;
        let fields = match scenario {
            Scenario::ExhaustedBudget => "",
            Scenario::PositiveInstruction => "Retry-After: 20\r\n",
        };
        respond(&mut signaling, &backup, &alternate, 503, fields, 1300).await?;
        let identity = signaling.registrations.identity;
        let client = signaling
            .registrations
            .clients
            .first()
            .ok_or(ControlError::Capacity)?;
        require(
            client.waits.deadline(primary_address, 1300) == Some(11_100),
            "Candidate exhaustion erased the primary retry deadline",
        )?;
        match scenario {
            Scenario::ExhaustedBudget => require(
                matches!(client.state, State::Stopped),
                "A missing backup instruction bypassed the exhausted retry budget",
            )?,
            Scenario::PositiveInstruction => require(
                matches!(client.state, State::CandidateWait { at: 11_100 }),
                "Positive peer instructions did not retain the earliest candidate wait",
            )?,
        }
        signaling.registration_tick(11_099).await?;
        let (primary_quiet, backup_quiet) = tokio::join!(silent(&primary), silent(&backup));
        cleanup::combine(primary_quiet, backup_quiet)?;
        require(
            signaling.registrations.identity == identity,
            "A non-due candidate wait reserved a sequence",
        )?;
        signaling.registration_tick(11_100).await?;
        match scenario {
            Scenario::ExhaustedBudget => {
                let (primary_quiet, backup_quiet) = tokio::join!(silent(&primary), silent(&backup));
                cleanup::combine(primary_quiet, backup_quiet)?;
                require(
                    signaling.registrations.identity == identity,
                    "An old peer hint restarted an exhausted registration",
                )
            }
            Scenario::PositiveInstruction => {
                let resumed =
                    receive(&primary, "Primary REGISTER at its retained deadline").await?;
                successor(&initial, &[&initial, &alternate], &resumed, 3)?;
                silent(&backup).await?;
                let contact = resumed.request.contact_header().map_err(failure)?.value();
                respond(
                    &mut signaling,
                    &primary,
                    &resumed,
                    200,
                    &format!("Contact: {contact};expires=11\r\n"),
                    11_200,
                )
                .await
            }
        }
    }
    .await;
    cleanup::combine(result, finish(signaling, local).await)
}

#[tokio::test]
async fn an_old_primary_hint_does_not_restart_a_registration_after_an_unhinted_backup_exhausts_its_budget(
) -> Result<(), ControlError> {
    run(Scenario::ExhaustedBudget).await
}

#[tokio::test]
async fn positive_backup_instructions_keep_the_earliest_peer_wait_without_configured_retry_budget(
) -> Result<(), ControlError> {
    run(Scenario::PositiveInstruction).await
}
