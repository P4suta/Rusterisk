use super::super::super::PreparedRegistrations;
use super::*;
use crate::reload::PublicationKey;
use rusterisk_core::{SettingsGeneration, SettingsLifetimes};
use rusterisk_sip::RegistrationTransactionEvent;

fn publication() -> Result<(PublicationKey, SettingsGeneration), ControlError> {
    let mut lifetimes = SettingsLifetimes::new(91);
    let base = lifetimes.current();
    let candidate = lifetimes.prepare().map_err(|_| ControlError::Capacity)?;
    let key = PublicationKey::new(candidate, base);
    let generation = lifetimes
        .commit(candidate)
        .map_err(|_| ControlError::Capacity)?
        .generation();
    Ok((key, generation))
}

#[tokio::test]
async fn quiescing_an_expired_original_preserves_its_deadline_processing(
) -> Result<(), ControlError> {
    let config = config(&["127.0.0.2:5070"], 0)?;
    let mut fixture = Fixture::new(config.clone()).await?;
    let primary = async {
        fixture.transmit(0, 1000)?;
        let prepared = PreparedRegistrations::prepare(&config)?;
        let (key, _) = publication()?;
        fixture.signaling.registrations.quiesce(
            key,
            &prepared,
            &fixture.signaling.network,
            33_000,
            38_000,
        )?;
        require(
            fixture.signaling.registrations.select(33_000) == Some(0),
            "Quiesce excluded the original transaction at its deadline",
        )?;
        require(
            fixture
                .signaling
                .registrations
                .poll(0, fixture.local, 33_000)?
                .is_none(),
            "Expired original transaction transmitted another packet",
        )?;
        require(
            matches!(
                fixture.signaling.registrations.clients[0].state,
                State::Backoff { .. }
            ),
            "Quiesce suppressed the original transaction timeout",
        )?;
        fixture.signaling.registrations.abort_publication(key)
    }
    .await;
    fixture.finish(primary).await
}

enum CompletionPolicy {
    Publish,
    Abort,
}

#[tokio::test]
async fn quiesce_defers_a_registered_refresh_until_abort() -> Result<(), ControlError> {
    let config = config(&["127.0.0.2:5070"], 0)?;
    let mut fixture = Fixture::new(config.clone()).await?;
    let primary = async {
        let original = fixture.transmit(0, 1000)?;
        let sent = request(&original)?;
        let contact = sent.contact_header().map_err(failure)?.value();
        let accepted = reply(&sent, 200, &format!("Contact: {contact};expires=60\r\n"))?;
        let State::Trying { transaction, .. } =
            &mut fixture.signaling.registrations.clients[0].state
        else {
            return Err(ControlError::Invalid("Original registration disappeared"));
        };
        let response = match transaction
            .receive(accepted.as_bytes(), original.peer, 1100)
            .map_err(failure)?
        {
            Some(RegistrationTransactionEvent::Response(response)) => response,
            Some(
                RegistrationTransactionEvent::Provisional(_)
                | RegistrationTransactionEvent::Transmit(_)
                | RegistrationTransactionEvent::TimedOut,
            )
            | None => {
                return Err(ControlError::Invalid("Registration grant was not consumed"));
            }
        };
        fixture
            .signaling
            .registrations
            .response(0, response, 1100)?;
        let State::Registered { refresh_at, .. } = fixture.signaling.registrations.clients[0].state
        else {
            return Err(ControlError::Invalid(
                "Registration grant did not retain a refresh",
            ));
        };
        let prepared = PreparedRegistrations::prepare(&config)?;
        let (key, _) = publication()?;
        fixture.signaling.registrations.quiesce(
            key,
            &prepared,
            &fixture.signaling.network,
            refresh_at,
            refresh_at.checked_add(5000).ok_or(ControlError::Capacity)?,
        )?;
        require(
            fixture.signaling.registrations.select(refresh_at).is_none()
                && fixture
                    .signaling
                    .registrations
                    .poll(0, fixture.local, refresh_at)?
                    .is_none(),
            "Quiesce sent a new refresh transaction",
        )?;
        fixture.signaling.registrations.abort_publication(key)?;
        let refreshed = fixture.transmit(0, refresh_at)?;
        require(
            refreshed.bytes != original.bytes,
            "Abort lost the pending refresh",
        )
    }
    .await;
    fixture.finish(primary).await
}

#[tokio::test]
async fn quiesce_continues_existing_retransmissions_and_defers_new_transactions(
) -> Result<(), ControlError> {
    for completion in [CompletionPolicy::Publish, CompletionPolicy::Abort] {
        let config = config(&["127.0.0.2:5070", "127.0.0.3:5070"], 0)?;
        let mut fixture = Fixture::new(config.clone()).await?;
        let primary = async {
            let original = fixture.transmit(0, 1000)?;
            let original_identity = fixture.signaling.registrations.identity;
            let prepared = PreparedRegistrations::prepare(&config)?;
            let (key, generation) = publication()?;
            fixture.signaling.registrations.quiesce(key, &prepared, &fixture.signaling.network, 1100, 6100)?;
            require(
                fixture.signaling.registrations.select(1500) == Some(0),
                "Quiesce suppressed an existing retransmission",
            )?;
            let repeated = fixture.transmit(0, 1500)?;
            require(repeated.bytes == original.bytes, "Existing retransmission changed its packet")?;
            require(
                fixture.signaling.registrations.poll(1, fixture.local, 1500)?.is_none(),
                "Quiesce sent a new ready transaction",
            )?;
            let challenged = reply(
                &request(&original)?,
                401,
                "WWW-Authenticate: Digest realm=\"old\", nonce=\"old\", algorithm=MD5, qop=\"auth\"\r\n",
            )?;
            let State::Trying { transaction, .. } = &mut fixture.signaling.registrations.clients[0].state else {
                return Err(ControlError::Invalid("Original registration disappeared"));
            };
            let response = match transaction.receive(challenged.as_bytes(), original.peer, 1600).map_err(failure)? {
                Some(RegistrationTransactionEvent::Response(response)) => response,
                Some(RegistrationTransactionEvent::Provisional(_)
                    | RegistrationTransactionEvent::Transmit(_)
                    | RegistrationTransactionEvent::TimedOut) | None => {
                    return Err(ControlError::Invalid("Original final response was not consumed"));
                }
            };
            fixture.signaling.registrations.response(0, response, 1600)?;
            require(
                fixture.signaling.registrations.identity != original_identity,
                "Existing challenge did not preserve its authentication continuation",
            )?;
            require(
                fixture.signaling.registrations.poll(0, fixture.local, 1600)?.is_none(),
                "Quiesce sent a newly authenticated transaction",
            )?;
            match completion {
                CompletionPolicy::Publish => {
                    fixture.signaling.registrations.publish(key, prepared, generation, 1700)?;
                    require(
                        fixture.signaling.registrations.retiring.len() == 0,
                        "Publication installed an already completed original transaction",
                    )?;
                    require(
                        fixture.signaling.registrations.select(2699).is_none(),
                        "Publication lost its registration activation floor",
                    )?;
                    let initial = fixture.transmit(0, 2700)?;
                    let initial = request(&initial)?;
                    require(
                        initial.authorization_header().is_none(),
                        "Replacement inherited the old authentication continuation",
                    )
                }
                CompletionPolicy::Abort => {
                    fixture.signaling.registrations.abort_publication(key)?;
                    let retry = fixture.transmit(0, 1700)?;
                    let retry = request(&retry)?;
                    require(
                        retry.authorization_header().is_some(),
                        "Abort overwrote the live authentication continuation",
                    )
                }
            }
        }.await;
        fixture.finish(primary).await?;
    }
    Ok(())
}
