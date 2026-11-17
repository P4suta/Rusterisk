use super::{PublicationKey, PublicationPhase, PublicationPort, PublicationState};
use crate::ControlError;
use rusterisk_core::{SettingsError, SettingsGeneration, SettingsLifetimes};

fn failure(error: SettingsError) -> ControlError {
    ControlError::Adapter(format!("Settings fixture failed: {error:?}"))
}

fn publication(owner: u128) -> Result<(PublicationKey, SettingsGeneration), ControlError> {
    let mut settings = SettingsLifetimes::new(owner);
    let base = settings.current();
    let candidate = settings.prepare().map_err(failure)?;
    let generation = settings.commit(candidate).map_err(failure)?.generation();
    Ok((PublicationKey::new(candidate, base), generation))
}

#[test]
fn publication_requires_quiescence_and_matching_generation() -> Result<(), ControlError> {
    let (key, generation) = publication(1)?;
    let (_, foreign_generation) = publication(2)?;
    let mut participant = PublicationState::new(key.base);
    participant.stage(key, String::from("captured payload"))?;
    assert!(participant.admitting());
    assert!(participant.publish(key, generation).is_err());
    participant.quiesce(key)?;
    assert!(!participant.admitting());
    assert_eq!(participant.publish(key, generation)?, "captured payload");
    assert!(participant.publish(key, generation).is_err());
    assert!(participant.resume(key, foreign_generation).is_err());
    assert!(!participant.admitting());
    assert!(participant.abort(key).is_err());
    participant.resume(key, generation)?;
    assert!(participant.admitting());
    assert!(participant
        .stage(key, String::from("stale payload"))
        .is_err());
    Ok(())
}

#[test]
fn stale_and_foreign_commands_preserve_the_staged_payload() -> Result<(), ControlError> {
    let (key, generation) = publication(1)?;
    let (foreign, _) = publication(2)?;
    let mut participant = PublicationState::new(key.base);
    assert!(participant.stage(foreign, String::from("foreign")).is_err());
    participant.stage(key, String::from("original"))?;
    assert_eq!(
        participant.stage(key, String::from("duplicate")),
        Err(ControlError::Capacity)
    );
    assert!(participant.quiesce(foreign).is_err());
    assert!(participant.abort(foreign).is_err());
    participant.quiesce(key)?;
    assert!(participant.publish(foreign, generation).is_err());
    assert_eq!(participant.publish(key, generation)?, "original");
    participant.resume(key, generation)
}

#[test]
fn a_foreign_candidate_with_a_local_base_preserves_the_valid_stage() -> Result<(), ControlError> {
    let (key, generation) = publication(1)?;
    let (foreign, _) = publication(2)?;
    let mixed = PublicationKey::new(foreign.candidate(), key.base);
    let mut participant = PublicationState::new(key.base);
    assert!(participant.stage(mixed, String::from("foreign")).is_err());
    participant.stage(key, String::from("captured local payload"))?;
    assert!(participant
        .stage(mixed, String::from("replacement"))
        .is_err());
    participant.quiesce(key)?;
    assert_eq!(
        participant.publish(key, generation)?,
        "captured local payload"
    );
    participant.resume(key, generation)
}

#[test]
fn foreign_and_other_candidate_generations_preserve_the_prepared_payload(
) -> Result<(), ControlError> {
    let mut settings = SettingsLifetimes::new(1);
    let base = settings.current();
    let candidate = settings.prepare().map_err(failure)?;
    let generation = settings.commit(candidate).map_err(failure)?.generation();
    let key = PublicationKey::new(candidate, base);
    let other_candidate = settings.prepare().map_err(failure)?;
    let other_generation = settings
        .commit(other_candidate)
        .map_err(failure)?
        .generation();
    let (_, foreign_generation) = publication(2)?;
    let mut participant = PublicationState::new(base);
    participant.stage(key, String::from("captured local payload"))?;
    participant.quiesce(key)?;
    assert!(participant.publish(key, foreign_generation).is_err());
    assert!(participant.publish(key, other_generation).is_err());
    assert!(!participant.admitting());
    assert_eq!(
        participant.publish(key, generation)?,
        "captured local payload"
    );
    participant.resume(key, generation)
}

#[test]
fn abort_releases_the_payload_without_publishing() -> Result<(), ControlError> {
    let mut settings = SettingsLifetimes::new(1);
    let base = settings.current();
    let first = settings.prepare().map_err(failure)?;
    let key = PublicationKey::new(first, base);
    let mut participant = PublicationState::new(key.base);
    participant.stage(key, String::from("discarded"))?;
    participant.quiesce(key)?;
    participant.abort(key)?;
    settings.cancel(first).map_err(failure)?;
    assert!(participant.admitting());
    let second = settings.prepare().map_err(failure)?;
    let replacement = PublicationKey::new(second, base);
    let generation = settings.commit(second).map_err(failure)?.generation();
    assert!(participant.publish(key, generation).is_err());
    participant.stage(replacement, String::from("replacement"))?;
    participant.quiesce(replacement)?;
    assert_eq!(participant.publish(replacement, generation)?, "replacement");
    participant.resume(replacement, generation)
}

#[tokio::test]
async fn phase_acknowledgements_use_independent_bounded_channels() -> Result<(), ControlError> {
    let (key, generation) = publication(1)?;
    let (port, mut receiver) = PublicationPort::channel();
    let mut state = PublicationState::new(key.base);
    let staged = port.stage(key, String::from("owned"))?;
    assert!(port.stage(key, String::from("overflow")).is_err());
    let prepared = receiver
        .prepared
        .recv()
        .await
        .ok_or(ControlError::Capacity)?;
    let result = state.stage(prepared.key, prepared.payload);
    prepared.reply.complete(result);
    staged.receive().await?;
    let quiesced = port.command(key, PublicationPhase::Quiesce)?;
    let command = receiver
        .commands
        .recv()
        .await
        .ok_or(ControlError::Capacity)?;
    assert_eq!(command.phase, PublicationPhase::Quiesce);
    let result = state.quiesce(command.key);
    command.reply.complete(result);
    quiesced.receive().await?;
    assert_eq!(state.publish(key, generation)?, "owned");
    state.resume(key, generation)
}

#[tokio::test]
async fn participant_failure_and_missing_acknowledgement_reject() -> Result<(), ControlError> {
    let (key, _) = publication(1)?;
    let (port, mut receiver) = PublicationPort::<()>::channel();
    let acknowledgement = port.command(key, PublicationPhase::Quiesce)?;
    let command = receiver
        .commands
        .recv()
        .await
        .ok_or(ControlError::Capacity)?;
    command.reply.complete(Err(ControlError::Capacity));
    assert_eq!(acknowledgement.receive().await, Err(ControlError::Capacity));
    let acknowledgement = port.command(key, PublicationPhase::Abort)?;
    let command = receiver
        .commands
        .recv()
        .await
        .ok_or(ControlError::Capacity)?;
    drop(command);
    assert!(acknowledgement.receive().await.is_err());
    Ok(())
}
