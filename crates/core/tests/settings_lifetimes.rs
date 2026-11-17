use rusterisk_core::{SettingsError, SettingsLifetimes};

#[test]
fn cancelled_candidates_cannot_publish_or_cancel_a_later_candidate() -> Result<(), SettingsError> {
    let mut settings = SettingsLifetimes::new(1);
    let initial = settings.current();
    let slot = settings.slot(initial)?;
    let cancelled = settings.prepare()?;
    assert_eq!(settings.current(), initial);
    assert_eq!(settings.slot(initial)?, slot);
    assert_eq!(settings.prepare(), Err(SettingsError::Capacity));
    settings.cancel(cancelled)?;
    let current = settings.prepare()?;
    assert_ne!(current, cancelled);
    assert!(settings.commit(cancelled).is_err());
    assert!(settings.cancel(cancelled).is_err());
    assert_eq!(settings.current(), initial);
    let published = settings.commit(current)?;
    assert_eq!(settings.current(), published.generation());
    assert_eq!(settings.slot(published.generation())?, published.slot());
    assert_ne!(published.generation(), initial);
    assert_eq!(published.retired_slot(), Some(slot));
    assert!(settings.slot(initial).is_err());
    Ok(())
}

#[test]
fn an_obsolete_slot_is_returned_only_after_its_last_reference_ends() -> Result<(), SettingsError> {
    let mut settings = SettingsLifetimes::new(1);
    let initial = settings.current();
    let initial_slot = settings.slot(initial)?;
    let first = settings.retain(initial)?;
    let second = settings.retain(initial)?;
    assert_eq!(first.generation(), initial);
    assert_eq!(second.generation(), initial);
    let candidate = settings.prepare()?;
    let published = settings.commit(candidate)?;
    assert_eq!(published.retired_slot(), None);
    assert_ne!(published.slot(), initial_slot);
    assert_eq!(settings.release(first)?, None);
    assert_eq!(settings.slot(initial)?, initial_slot);
    assert_eq!(settings.release(second)?, Some(initial_slot));
    assert!(settings.slot(initial).is_err());
    assert!(settings.retain(initial).is_err());
    assert_eq!(settings.current(), published.generation());
    let current = settings.retain(published.generation())?;
    assert_eq!(current.generation(), published.generation());
    assert_eq!(settings.release(current)?, None);
    assert_eq!(settings.slot(published.generation())?, published.slot());
    Ok(())
}

#[test]
fn every_started_slot_survives_saturation_and_publication_resumes_after_release(
) -> Result<(), SettingsError> {
    let mut settings = SettingsLifetimes::new(1);
    let mut started = Vec::with_capacity(65);
    for _ in 0..65 {
        let current = settings.current();
        let slot = settings.slot(current)?;
        let reference = settings.retain(current)?;
        assert_eq!(reference.generation(), current);
        started.push((reference, slot));
        if started.len() < 65 {
            let candidate = settings.prepare()?;
            settings.commit(candidate)?;
        }
    }
    let current = settings.current();
    assert_eq!(settings.prepare(), Err(SettingsError::Capacity));
    assert_eq!(settings.current(), current);
    for (reference, slot) in &started {
        assert_eq!(settings.slot(reference.generation())?, *slot);
    }
    let mut started = started.into_iter();
    let (reference, freed_slot) = started.next().ok_or(SettingsError::Capacity)?;
    let released = reference.generation();
    assert_eq!(settings.release(reference)?, Some(freed_slot));
    let candidate = settings.prepare()?;
    let replacement = settings.commit(candidate)?;
    assert_eq!(replacement.slot(), freed_slot);
    assert_eq!(replacement.retired_slot(), None);
    assert_ne!(replacement.generation(), released);
    assert!(settings.slot(released).is_err());
    assert!(settings.retain(released).is_err());
    assert_eq!(settings.slot(replacement.generation())?, freed_slot);
    for (reference, slot) in started {
        assert_eq!(settings.slot(reference.generation())?, slot);
        assert_eq!(settings.release(reference)?, Some(slot));
    }
    Ok(())
}

#[test]
fn publication_rechecks_a_slot_retained_after_candidate_preparation() -> Result<(), SettingsError> {
    let mut settings = SettingsLifetimes::new(1);
    let mut started = Vec::with_capacity(65);
    for _ in 0..64 {
        let current = settings.current();
        let reference = settings.retain(current)?;
        assert_eq!(reference.generation(), current);
        started.push(reference);
        let candidate = settings.prepare()?;
        settings.commit(candidate)?;
    }
    let current = settings.current();
    let slot = settings.slot(current)?;
    let candidate = settings.prepare()?;
    let reference = settings.retain(current)?;
    assert_eq!(reference.generation(), current);
    started.push(reference);
    assert!(matches!(
        settings.commit(candidate),
        Err(SettingsError::Capacity)
    ));
    assert_eq!(settings.current(), current);
    assert_eq!(settings.slot(current)?, slot);
    for reference in &started {
        assert!(settings.slot(reference.generation()).is_ok());
    }
    settings.cancel(candidate)?;
    for reference in started {
        settings.release(reference)?;
    }
    assert_eq!(settings.slot(current)?, slot);
    Ok(())
}
