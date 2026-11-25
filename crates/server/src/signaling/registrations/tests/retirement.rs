use super::super::{
    resolution::{Completion, ConfigurationLifetime},
    retiring::Retired,
    ConfigurationKey, PreparedRegistrations, Registrations, Route, State,
};
use super::{config, stream::reply};
use crate::{
    media::MediaHub,
    reload::PublicationKey,
    signaling::{cleanup, resolver::ResolvedTarget, Received, Signaling},
    ControlError, Controller,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, SipMessage};
use rusterisk_config::{Config, MAX_REGISTRATIONS};
use rusterisk_core::{SettingsError, SettingsLifetimes};
use rusterisk_sip::{Datagram, RegistrationRetirement, StaticTarget};
use rusterisk_test_support::Wait;
use std::{net::SocketAddr, num::NonZeroU64};
use tokio::{net::UdpSocket, sync::oneshot};

mod quiesce;

const SLOTS: usize = 16;

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

fn request(packet: &Datagram) -> Result<Request, ControlError> {
    match SipMessage::try_from(packet.bytes.as_slice()).map_err(failure)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ControlError::Invalid("Expected a REGISTER request")),
    }
}

fn metadata(
    retirement: &RegistrationRetirement,
    registrar: &StaticTarget,
    route: &crate::signaling::transport::DeliveryRoute,
    deadline: u64,
) -> Result<(), ControlError> {
    require(
        retirement.expires_at() == deadline
            && registrar.protocol() == rusterisk_config::Protocol::Udp
            && route.listener() == 0
            && route.peer().port() == 5070,
        "Retirement changed its starting route, registrar, or original deadline",
    )
}

fn metadata_only(manager: &Registrations, deadline: u64) -> Result<(), ControlError> {
    let slots: &[Option<Retired>; SLOTS] = &manager.retiring.slots;
    for entry in slots.iter().flatten() {
        let Retired {
            retirement,
            registrar,
            route,
        } = entry;
        metadata(retirement, registrar, route, deadline)?;
    }
    Ok(())
}

fn settings_failure(error: SettingsError) -> ControlError {
    failure(format!("Settings publication failed: {error:?}"))
}

struct Fixture {
    signaling: Signaling,
    settings: SettingsLifetimes,
    local: SocketAddr,
}

impl Fixture {
    async fn new(config: Config) -> Result<Self, ControlError> {
        let (controller, runner, _effects) = Controller::new(config.clone(), 16);
        let (media, _hub, notices) = MediaHub::new(16);
        let signaling = Signaling::bind(config, controller, runner, media, notices).await?;
        let local = signaling.network.local_addr(0)?;
        Ok(Self {
            signaling,
            settings: SettingsLifetimes::new(93),
            local,
        })
    }

    fn transmit(&mut self, index: usize, now: u64) -> Result<Datagram, ControlError> {
        self.signaling
            .registrations
            .poll(index, self.local, now)?
            .ok_or(ControlError::Invalid("Expected an initial REGISTER"))
    }

    fn publish(&mut self, config: &Config, now: u64) -> Result<(), ControlError> {
        let prepared = PreparedRegistrations::prepare(config)?;
        let base = self.settings.current();
        let candidate = self.settings.prepare().map_err(settings_failure)?;
        let key = PublicationKey::new(candidate, base);
        let quiesced =
            self.signaling
                .registrations
                .quiesce(key, &prepared, &self.signaling.network, now, now);
        if let Err(primary) = quiesced {
            return cleanup::combine(
                Err(primary),
                self.settings.cancel(candidate).map_err(settings_failure),
            );
        }
        let generation = match self.settings.commit(candidate) {
            Ok(published) => published.generation(),
            Err(error) => {
                let result = cleanup::combine(
                    Err(settings_failure(error)),
                    self.signaling.registrations.abort_publication(key),
                );
                return cleanup::combine(
                    result,
                    self.settings.cancel(candidate).map_err(settings_failure),
                );
            }
        };
        self.signaling
            .registrations
            .publish(key, prepared, generation, now)
    }

    fn receive(
        &mut self,
        packet: &Datagram,
        status: u16,
        extra: &str,
        now: u64,
    ) -> Result<bool, ControlError> {
        self.receive_from(packet, packet.peer, status, extra, now)
    }

    fn receive_from(
        &mut self,
        packet: &Datagram,
        peer: SocketAddr,
        status: u16,
        extra: &str,
        now: u64,
    ) -> Result<bool, ControlError> {
        let received = Received {
            listener: 0,
            peer,
            route: self.signaling.network.capture(0, peer)?,
            bytes: reply(&request(packet)?, status, extra)?.into_bytes(),
        };
        Ok(self.signaling.registration_received(&received, now))
    }

    async fn finish(mut self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let result = cleanup::combine(primary, self.signaling.finish_registrations().await);
        let result = cleanup::combine(result, self.signaling.finish_native().await);
        let result = cleanup::combine(result, self.signaling.network.close().await);
        let local = self.local;
        drop(self);
        cleanup::combine(
            result,
            UdpSocket::bind(local)
                .await
                .map(|_socket| ())
                .map_err(failure),
        )
    }
}

#[tokio::test]
async fn initial_activation_uses_publication_time_and_a_checked_one_second_floor(
) -> Result<(), ControlError> {
    for delay in [0, 1, 10, u32::MAX] {
        let mut fixture = Fixture::new(config(&[], 0)?).await?;
        let primary = (|| {
            fixture.publish(&config(&["127.0.0.2:5070"], delay)?, 60_000)?;
            let manager = &mut fixture.signaling.registrations;
            let State::Ready { at, .. } = manager.clients[0].state else {
                return Err(ControlError::Invalid("Expected a literal UDP destination"));
            };
            require(
                (61_000..=60_000 + u64::from(delay.max(1)) * 1000).contains(&at),
                "Initial activation was not anchored to publication",
            )?;
            require(
                manager.select(60_000).is_none(),
                "Publication sent an immediate REGISTER",
            )?;
            require(
                manager.select(60_999).is_none(),
                "Initial activation lost its one second floor",
            )?;
            if delay == 0 {
                require(
                    at == 61_000,
                    "Zero random delay changed the activation floor",
                )?;
                require(
                    manager.select(61_000) == Some(0),
                    "Initial activation missed its exact deadline",
                )?;
            }
            Ok(())
        })();
        fixture.finish(primary).await?;
    }
    require(
        matches!(
            Registrations::new_at(&config(&["127.0.0.2:5070"], 0)?, u64::MAX - 999),
            Err(ControlError::Capacity)
        ),
        "Initial activation overflow was admitted",
    )
}

#[tokio::test]
async fn clock_and_generation_overflow_preserve_the_current_owner_before_cancellation(
) -> Result<(), ControlError> {
    for generation_overflow in [false, true] {
        let original = config(&["127.0.0.2:5070"], 0)?;
        let mut fixture = Fixture::new(original.clone()).await?;
        let primary = async {
            let packet = fixture.transmit(0, 1000)?;
            if generation_overflow {
                fixture.signaling.registrations.lifetime =
                    ConfigurationLifetime::new(NonZeroU64::MAX);
            }
            let generation = fixture.signaling.registrations.generation();
            let lease = fixture.signaling.registrations.lifetime.lease();
            let identity = fixture.signaling.registrations.identity;
            let cursor = fixture.signaling.registrations.cursor;
            let now = if generation_overflow {
                1500
            } else {
                u64::MAX - 999
            };
            require(
                matches!(fixture.publish(&original, now), Err(ControlError::Capacity)),
                "Replacement overflow was admitted",
            )?;
            require(
                fixture.signaling.registrations.generation() == generation && !lease.is_released(),
                "Rejected replacement changed its lifetime",
            )?;
            require(
                fixture.signaling.registrations.identity == identity
                    && fixture.signaling.registrations.cursor == cursor,
                "Rejected replacement changed request allocation",
            )?;
            require(
                fixture.signaling.registrations.retiring.len() == 0
                    && fixture.signaling.registrations.setup_count() == 0,
                "Rejected replacement installed retiring or setup work",
            )?;
            let State::Trying { transaction, .. } =
                &fixture.signaling.registrations.clients[0].state
            else {
                return Err(ControlError::Invalid(
                    "Rejected replacement cancelled the current transaction",
                ));
            };
            require(
                transaction.identity() == identity && transaction.expires_at() == 33_000,
                "Rejected replacement changed its current transaction",
            )?;
            let repeated = fixture.transmit(0, 1500)?;
            require(
                repeated.bytes == packet.bytes,
                "Rejected replacement changed the next retransmission",
            )
        }
        .await;
        fixture.finish(primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn retirement_blocks_its_registrar_but_preserves_other_registration_progress(
) -> Result<(), ControlError> {
    let original = config(&["127.0.0.2:5070"], 0)?;
    let mut fixture = Fixture::new(original).await?;
    let primary = async {
        let packet = fixture.transmit(0, 1000)?;
        let initial = request(&packet)?;
        let lease = fixture.signaling.registrations.lifetime.lease();
        fixture.publish(&config(&["127.0.0.2:5070", "127.0.0.3:5070"], 0)?, 1500)?;
        require(
            lease.is_released() && fixture.signaling.registrations.generation().get() == 2,
            "Replacement did not end the previous generation",
        )?;
        require(
            fixture.signaling.registrations.retiring.len() == 1,
            "Replacement lost an outstanding transaction",
        )?;
        metadata_only(&fixture.signaling.registrations, 33_000)?;
        require(
            fixture.signaling.registrations.select(2500) == Some(1),
            "Retirement blocked an unrelated registrar or admitted its own",
        )?;
        require(
            fixture
                .signaling
                .registrations
                .poll(0, fixture.local, 2500)?
                .is_none(),
            "Direct polling bypassed the retired registrar barrier",
        )?;
        fixture.transmit(1, 2500)?;
        let identity = fixture.signaling.registrations.identity;
        require(
            !fixture.receive_from(
                &packet,
                "127.0.0.99:5070".parse().map_err(failure)?,
                200,
                "",
                2550,
            )? && fixture.signaling.registrations.retiring.len() == 1
                && fixture.signaling.registrations.identity == identity,
            "A final response from another peer settled the old registration",
        )?;
        require(
            fixture.receive(&packet, 100, "", 2600)?,
            "Retirement lost a matching provisional response",
        )?;
        require(
            fixture.signaling.registrations.retiring.len() == 1
                && fixture.signaling.registrations.select(2600).is_none(),
            "A provisional response released registrar serialization",
        )?;
        require(
            fixture.receive(&packet, 200, "", 2601)?,
            "Retirement did not settle its matching final response",
        )?;
        require(
            fixture.signaling.registrations.retiring.len() == 0
                && fixture.signaling.registrations.select(2601) == Some(0),
            "Final settlement did not release the registrar",
        )?;
        let next = request(&fixture.transmit(0, 2601)?)?;
        require(
            next.call_id_header().map_err(failure)?.value()
                == initial.call_id_header().map_err(failure)?.value(),
            "Replacement changed the boot Call-ID",
        )?;
        require(
            next.cseq_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .seq
                > initial
                    .cseq_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?
                    .seq,
            "Replacement reused a completed request sequence",
        )?;
        require(
            next.top_via_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .branch()
                != initial
                    .top_via_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?
                    .branch(),
            "Replacement reused the retired Via branch",
        )?;
        require(
            !fixture.receive(&packet, 200, "", 2602)?,
            "A settled retirement revived on a duplicate final response",
        )
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn the_same_named_registrar_stays_blocked_when_its_selected_address_changes(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new(config(&["provider.test:5070"], 0)?).await?;
    let primary = async {
        fixture.signaling.registrations.clients[0].state = State::Ready {
            at: 1000,
            route: Route::Datagram("127.0.0.2:5070".parse().map_err(failure)?),
        };
        fixture.transmit(0, 1000)?;
        fixture.publish(
            &config(&["PROVIDER.test.:5070", "other.test:5070"], 0)?,
            1500,
        )?;
        for (client, peer) in fixture
            .signaling
            .registrations
            .clients
            .iter_mut()
            .zip(["127.0.0.99:5070", "127.0.0.3:5070"])
        {
            client.state = State::Ready {
                at: 2500,
                route: Route::Datagram(peer.parse().map_err(failure)?),
            };
        }
        require(
            fixture.signaling.registrations.select(2500) == Some(1),
            "A changed address bypassed the named registrar barrier",
        )?;
        require(
            fixture
                .signaling
                .registrations
                .poll(0, fixture.local, 2500)?
                .is_none(),
            "Direct polling bypassed the named registrar barrier",
        )?;
        fixture.transmit(1, 2500)?;
        fixture.signaling.registrations.retiring.poll(32_999)?;
        require(
            fixture.signaling.registrations.retiring.len() == 1,
            "Address selection changed the retirement deadline",
        )?;
        fixture.signaling.registrations.retiring.poll(33_000)?;
        let packet = fixture.transmit(0, 33_000)?;
        require(
            packet.peer == "127.0.0.99:5070".parse().map_err(failure)?,
            "The released registrar lost its replacement address",
        )
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn the_original_deadline_releases_retirements_even_without_current_clients(
) -> Result<(), ControlError> {
    for empty in [false, true] {
        let original = config(&["127.0.0.2:5070"], 0)?;
        let mut fixture = Fixture::new(original.clone()).await?;
        let primary = async {
            fixture.transmit(0, 1000)?;
            fixture.publish(
                &if empty {
                    config(&[], 0)?
                } else {
                    original.clone()
                },
                1500,
            )?;
            fixture.signaling.registrations.retiring.poll(32_999)?;
            require(
                fixture.signaling.registrations.retiring.len() == 1,
                "Retirement expired before the original deadline",
            )?;
            metadata_only(&fixture.signaling.registrations, 33_000)?;
            require(
                fixture.signaling.registrations.select(32_999).is_none(),
                "An unfinished retirement admitted the same registrar",
            )?;
            fixture.signaling.registrations.retiring.poll(33_000)?;
            require(
                fixture.signaling.registrations.retiring.len() == 0,
                "Retirement extended its original deadline",
            )?;
            if !empty {
                require(
                    fixture.signaling.registrations.select(33_000) == Some(0),
                    "Deadline settlement did not release registrar serialization",
                )?;
                fixture.transmit(0, 33_000)?;
            }
            let identity = fixture.signaling.registrations.identity;
            fixture.signaling.registrations.retiring.poll(u64::MAX)?;
            require(
                fixture.signaling.registrations.retiring.len() == 0
                    && fixture.signaling.registrations.identity == identity,
                "Released retirement restarted request allocation",
            )
        }
        .await;
        fixture.finish(primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn retired_challenges_corrections_and_failures_cannot_restart_the_previous_client(
) -> Result<(), ControlError> {
    for (status, extra) in [
        (401, "WWW-Authenticate: Digest realm=\"old\", nonce=\"old\", algorithm=MD5, qop=\"auth\"\r\n"),
        (407, "Proxy-Authenticate: Digest realm=\"old\", nonce=\"old\", algorithm=MD5, qop=\"auth\"\r\n"),
        (423, "Min-Expires: 60\r\n"),
        (503, "Retry-After: 10\r\n"),
    ] {
        let original = config(&["127.0.0.2:5070"], 0)?;
        let mut replacement = original.clone();
        replacement.registrations[0].expiration = 5;
        replacement.auth[0].username = "replacement-account".into();
        let mut fixture = Fixture::new(original).await?;
        let primary = async {
            let packet = fixture.transmit(0, 1000)?;
            fixture.publish(&replacement, 1500)?;
            let identity = fixture.signaling.registrations.identity;
            require(fixture.receive(&packet, status, extra, 1600)?, "Retirement failed to settle an old final status")?;
            require(fixture.signaling.registrations.retiring.len() == 0 && fixture.signaling.registrations.identity == identity, "Retired final status allocated an authentication or correction request")?;
            require(fixture.signaling.registrations.clients[0].requested_expiration == 5 && fixture.signaling.registrations.clients[0].retries == 0, "Retired final status changed replacement retry or expiration policy")?;
            require(fixture.signaling.registrations.select(2499).is_none(), "Retired final status bypassed replacement activation")?;
            let packet = fixture.transmit(0, 2500)?;
            let request = request(&packet)?;
            require(request.headers.iter().all(|header| !["Authorization", "Proxy-Authorization"].iter().any(|name| header.name().eq_ignore_ascii_case(name))), "Replacement inherited retired credentials")?;
            let expiration = request.headers.iter().any(|header| header.name() == "Expires" && header.value() == "5");
            require(expiration, "Replacement inherited an old minimum expiration")
        }.await;
        fixture.finish(primary).await?;
    }
    Ok(())
}

#[tokio::test]
async fn sixteen_retirements_refuse_replacement_without_touching_current_jobs_or_lifetimes(
) -> Result<(), ControlError> {
    let old = (2..=17)
        .map(|index| format!("127.0.0.{index}:5070"))
        .collect::<Vec<_>>();
    let next = (2..=17)
        .map(|index| format!("127.0.1.{index}:5070"))
        .collect::<Vec<_>>();
    let old_refs = old.iter().map(String::as_str).collect::<Vec<_>>();
    let next_refs = next.iter().map(String::as_str).collect::<Vec<_>>();
    let replacement = config(&next_refs, 0)?;
    let mut fixture = Fixture::new(config(&old_refs, 0)?).await?;
    let primary = async {
        require(MAX_REGISTRATIONS == SLOTS, "The registration and retirement limits diverged")?;
        for index in 0..SLOTS {
            fixture.transmit(index, 1000)?;
        }
        fixture.publish(&replacement, 1500)?;
        require(fixture.signaling.registrations.retiring.len() == SLOTS, "A full active generation was not retained")?;
        metadata_only(&fixture.signaling.registrations, 33_000)?;
        let packet = fixture.transmit(0, 2500)?;
        let generation = fixture.signaling.registrations.generation();
        let lease = fixture.signaling.registrations.lifetime.lease();
        let identity = fixture.signaling.registrations.identity;
        let cursor = fixture.signaling.registrations.cursor;
        let setup = fixture.signaling.network.reserve_setup(0, generation)?;
        let key = ConfigurationKey { generation, index: 1 };
        let target = fixture.signaling.registrations.clients[1].target.clone();
        let job_lease = lease.clone();
        let (resume, resumed) = oneshot::channel();
        fixture.signaling.registrations.clients[1].state = State::Resolving { setup };
        fixture.signaling.registrations.jobs.spawn(async move {
            let result = tokio::select! {
                biased;
                _ended = job_lease.released() => Err(ControlError::Conflict("Current resolution was cancelled")),
                resumed = resumed => resumed.map_err(failure).and_then(|()| ResolvedTarget::literal(&target)?.ok_or(ControlError::Invalid("Expected a literal resolution"))),
            };
            Completion { key, setup, lease: job_lease, result }
        });
        require(matches!(fixture.publish(&replacement, 2600), Err(ControlError::Capacity)), "Replacement evicted an occupied retirement slot")?;
        require(!lease.is_released() && fixture.signaling.registrations.generation() == generation, "Capacity refusal cancelled or published a generation")?;
        require(fixture.signaling.registrations.identity == identity && fixture.signaling.registrations.cursor == cursor && fixture.signaling.registrations.setup_count() == 1, "Capacity refusal changed current request or job ownership")?;
        require(fixture.signaling.registrations.retiring.len() == SLOTS && matches!(fixture.signaling.registrations.clients[0].state, State::Trying { .. }) && matches!(fixture.signaling.registrations.clients[1].state, State::Resolving { setup: active } if active == setup), "Capacity refusal changed current or retired clients")?;
        resume.send(()).map_err(|()| ControlError::Invalid("Current resolution stopped before its release"))?;
        let completed = Wait::Ready.bounded(fixture.signaling.registrations.completed()).await.map_err(failure)??.ok_or(ControlError::Invalid("Missing current resolution completion"))?;
        fixture.signaling.network.finish_resolution(completed.setup);
        require(completed.result.is_ok() && fixture.signaling.registrations.lifetime.is_current(completed.key, &completed.lease), "Rejected replacement cancelled or invalidated current work")?;
        let repeated = fixture.transmit(0, 3000)?;
        require(repeated.bytes == packet.bytes, "Capacity refusal changed an outstanding REGISTER")?;
        metadata_only(&fixture.signaling.registrations, 33_000)
    }.await;
    fixture.finish(primary).await
}
