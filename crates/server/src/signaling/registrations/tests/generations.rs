use super::{config, dns, ControlError};
use crate::{
    media::MediaHub,
    signaling::{
        cleanup,
        registrations::{resolution::ConfigurationLifetime, State},
        Signaling,
    },
    CallKey, Controller,
};
use hickory_resolver::proto::{
    op::{Message, OpCode},
    rr::{rdata::A, RData, Record},
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use std::{net::Ipv4Addr, num::NonZeroU64, time::Duration};
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

async fn signaling(config: Config, dns: &UdpSocket) -> Result<Signaling, ControlError> {
    let (controller, _runner, effects) = Controller::new(config.clone(), 64);
    let (media, _hub, notices) = MediaHub::new(64);
    let mut signaling = Signaling::bind(config, controller, effects, media, notices).await?;
    signaling.resolver = Ok(dns::resolver(dns)?);
    Ok(signaling)
}

async fn finish(
    mut signaling: Signaling,
    primary: Result<(), ControlError>,
) -> Result<(), ControlError> {
    let mut result = cleanup::combine(primary, signaling.finish_registrations().await);
    result = cleanup::combine(result, signaling.finish_native().await);
    cleanup::combine(result, signaling.network.close().await)
}

async fn reply(dns: &UdpSocket) -> Result<(), ControlError> {
    let (bytes, peer) = dns::receive(dns).await?;
    let query = Message::from_vec(&bytes).map_err(failure)?;
    let question = query.queries.first().ok_or(ControlError::Capacity)?.clone();
    let mut response = Message::response(query.metadata.id, OpCode::Query);
    response.metadata.authoritative = true;
    response.metadata.recursion_available = true;
    response.metadata.recursion_desired = query.metadata.recursion_desired;
    response.queries.push(question.clone());
    response.answers.push(Record::from_rdata(
        question.name().clone(),
        60,
        RData::A(A(Ipv4Addr::LOCALHOST)),
    ));
    response.edns = query.edns;
    dns::send(dns, &response.to_vec().map_err(failure)?, peer).await
}

#[tokio::test]
async fn released_or_previous_configuration_keys_cannot_cache_or_transmit_completed_resolution(
) -> Result<(), ControlError> {
    for previous_key in [false, true] {
        let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let registrar = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let remote = registrar.local_addr().map_err(failure)?;
        let config = config(&[&format!("provider.test:{}", remote.port())], 0)?;
        let mut signaling = signaling(config, &dns).await?;
        let primary: Result<(), ControlError> = async {
            let target = signaling.registrations.clients[0].target.clone();
            signaling.registration_tick(1000).await?;
            reply(&dns).await?;
            let mut completed =
                tokio::time::timeout(Duration::from_secs(3), signaling.registrations.completed())
                    .await
                    .map_err(failure)??
                    .ok_or(ControlError::Capacity)?;
            if completed.result.is_err() {
                return Err(ControlError::Invalid("Expected successful DNS completion"));
            }
            if previous_key {
                signaling.registrations.lifetime =
                    ConfigurationLifetime::new(NonZeroU64::new(2).ok_or(ControlError::Capacity)?);
                completed.lease = signaling.registrations.lifetime.lease();
            } else {
                signaling.registrations.lifetime.cancel();
            }
            signaling.registration_completed(completed, 1001)?;
            if signaling
                .resolution_cache
                .get(&target, std::time::Instant::now())
                .is_some()
                || !matches!(
                    signaling.registrations.clients[0].state,
                    State::Resolving { .. }
                )
            {
                return Err(ControlError::Invalid(
                    "Stale completion changed configuration state",
                ));
            }
            signaling.registration_tick(1001).await?;
            if tokio::time::timeout(
                Duration::from_millis(100),
                registrar.recv_from(&mut [0; 16_385]),
            )
            .await
            .is_ok()
            {
                return Err(ControlError::Invalid(
                    "Stale resolution sent registration bytes",
                ));
            }
            Ok(())
        }
        .await;
        finish(signaling, primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn cancelling_configuration_reaps_unanswered_resolution_without_a_dns_response(
) -> Result<(), ControlError> {
    let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
    let mut signaling = signaling(config(&["provider.test:5070"], 0)?, &dns).await?;
    let primary: Result<(), ControlError> = async {
        signaling.registration_tick(1000).await?;
        dns::receive(&dns).await?;
        tokio::time::timeout(Duration::from_secs(1), signaling.finish_registrations())
            .await
            .map_err(failure)??;
        if signaling.registrations.setup_count() != 0
            || signaling.registrations.select(u64::MAX).is_some()
        {
            return Err(ControlError::Invalid(
                "Cancelled resolution retained registration work",
            ));
        }
        Ok(())
    }
    .await;
    finish(signaling, primary).await
}

fn capacity_config() -> Result<Config, ControlError> {
    let mut pjsip = "[native]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[account]\ntype=auth\nusername=account\npassword=sample\n[static]\ntype=aor\ncontact=sip:phone@127.0.0.2:5071\n[phone]\ntype=endpoint\ntransport=native\nauth=account\naors=static\nallow=ulaw\n".to_owned();
    for index in 0..16 {
        pjsip.push_str(&format!("[provider-{index}]\ntype=registration\ntransport=native\noutbound_auth=account\nserver_uri=sip:provider-{index}.test:5070\nclient_uri=sip:account{index}@provider.example\nmax_random_initial_delay=0\n"));
    }
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &pjsip,
        "",
        "",
    )
    .map_err(failure)
}

fn call_key(index: u64) -> Result<CallKey, ControlError> {
    Ok(CallKey {
        id: ChannelId::try_from(format!("registration-budget-{index}"))?,
        generation: index,
    })
}

#[tokio::test]
async fn calls_and_registration_resolution_share_sixteen_slots_until_work_is_reaped(
) -> Result<(), ControlError> {
    for calls_first in [false, true] {
        let dns = UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?;
        let mut signaling = signaling(capacity_config()?, &dns).await?;
        let primary: Result<(), ControlError> = async {
            if calls_first {
                for index in 1..=16 {
                    signaling.queue_native(
                        call_key(index)?,
                        "PJSIP/phone".into(),
                        "100".into(),
                        None,
                        0,
                    )?;
                }
                signaling.registration_tick(1000).await?;
                if signaling.native_pending.len() != 16
                    || signaling.registrations.setup_count() != 0
                    || !matches!(
                        signaling.registrations.clients[0].state,
                        State::Waiting { .. }
                    )
                {
                    return Err(ControlError::Invalid(
                        "Registration exceeded the shared setup limit",
                    ));
                }
                if tokio::time::timeout(Duration::from_millis(100), dns.recv_from(&mut [0; 2048]))
                    .await
                    .is_ok()
                {
                    return Err(ControlError::Invalid(
                        "Deferred registration started DNS work",
                    ));
                }
            } else {
                for _ in 0..16 {
                    signaling.registration_tick(1000).await?;
                }
                if signaling.registrations.setup_count() != 16
                    || !matches!(
                        signaling.queue_native(
                            call_key(1)?,
                            "PJSIP/phone".into(),
                            "100".into(),
                            None,
                            0
                        ),
                        Err(ControlError::Capacity)
                    )
                {
                    return Err(ControlError::Invalid(
                        "Call exceeded the shared setup limit",
                    ));
                }
                signaling.registrations.cancel();
                if !matches!(
                    signaling.queue_native(
                        call_key(2)?,
                        "PJSIP/phone".into(),
                        "100".into(),
                        None,
                        0
                    ),
                    Err(ControlError::Capacity)
                ) {
                    return Err(ControlError::Invalid(
                        "Cancelled jobs released capacity before reaping",
                    ));
                }
                signaling.finish_registrations().await?;
                signaling.queue_native(
                    call_key(3)?,
                    "PJSIP/phone".into(),
                    "100".into(),
                    None,
                    0,
                )?;
            }
            Ok(())
        }
        .await;
        finish(signaling, primary).await?;
    }
    Ok(())
}
