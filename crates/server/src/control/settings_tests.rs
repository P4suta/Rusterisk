use super::settings::SettingsGenerations;
use crate::ControlError;

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    transport: String,
    music: String,
}

fn snapshot(name: &str) -> Snapshot {
    Snapshot {
        transport: format!("transport-{name}"),
        music: format!("music-{name}"),
    }
}

#[test]
fn preparing_and_cancelling_a_candidate_preserves_the_published_settings(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let candidate = settings.prepare(snapshot("replacement"))?;
    assert_eq!(settings.current(), initial);
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    settings.cancel(candidate)?;
    assert_eq!(settings.current(), initial);
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    Ok(())
}

#[test]
fn committing_changes_only_the_current_generation_and_keeps_each_started_snapshot(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let reference = settings.retain(initial)?;
    assert_eq!(reference.generation(), initial);
    let candidate = settings.prepare(snapshot("replacement"))?;
    let replacement = settings.commit(candidate)?;
    assert_ne!(replacement, initial);
    assert_eq!(settings.current(), replacement);
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    assert_eq!(settings.get(replacement)?, &snapshot("replacement"));
    settings.release(reference)?;
    assert!(settings.get(initial).is_err());
    assert_eq!(settings.get(replacement)?, &snapshot("replacement"));
    Ok(())
}

#[test]
fn every_call_reference_must_end_before_an_obsolete_snapshot_is_released(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let first = settings.retain(initial)?;
    let second = settings.retain(initial)?;
    assert_eq!(first.generation(), initial);
    assert_eq!(second.generation(), initial);
    let candidate = settings.prepare(snapshot("replacement"))?;
    let replacement = settings.commit(candidate)?;
    settings.release(first)?;
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    settings.release(second)?;
    assert!(settings.get(initial).is_err());
    assert!(settings.retain(initial).is_err());
    assert_eq!(settings.current(), replacement);
    assert_eq!(settings.get(replacement)?, &snapshot("replacement"));
    Ok(())
}

#[test]
fn an_unreferenced_current_snapshot_survives_until_publication() -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let reference = settings.retain(initial)?;
    assert_eq!(reference.generation(), initial);
    settings.release(reference)?;
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    let candidate = settings.prepare(snapshot("replacement"))?;
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    let replacement = settings.commit(candidate)?;
    assert!(settings.get(initial).is_err());
    assert_eq!(settings.get(replacement)?, &snapshot("replacement"));
    Ok(())
}

#[test]
fn a_second_candidate_is_refused_without_changing_the_pending_or_current_settings(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let reference = settings.retain(initial)?;
    assert_eq!(reference.generation(), initial);
    let selected = settings.prepare(snapshot("selected"))?;
    assert!(matches!(
        settings.prepare(snapshot("overflow")),
        Err(ControlError::Capacity)
    ));
    let current = settings.commit(selected)?;
    assert_eq!(settings.current(), current);
    assert_eq!(settings.get(current)?, &snapshot("selected"));
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    settings.release(reference)?;
    Ok(())
}

#[test]
fn an_old_candidate_key_cannot_commit_or_cancel_its_replacement() -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let old = settings.prepare(snapshot("cancelled"))?;
    settings.cancel(old)?;
    let current = settings.prepare(snapshot("selected"))?;
    assert!(settings.commit(old).is_err());
    assert!(settings.cancel(old).is_err());
    assert_eq!(settings.current(), initial);
    assert_eq!(settings.get(initial)?, &snapshot("initial"));
    assert!(matches!(
        settings.prepare(snapshot("overflow")),
        Err(ControlError::Capacity)
    ));
    let published = settings.commit(current)?;
    assert_eq!(settings.get(published)?, &snapshot("selected"));
    assert!(settings.get(initial).is_err());
    assert!(settings.commit(old).is_err());
    assert!(settings.cancel(old).is_err());
    assert_eq!(settings.current(), published);
    Ok(())
}

#[test]
fn saturation_refuses_preparation_before_changing_or_releasing_any_generation(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let mut started = Vec::with_capacity(65);
    let initial = settings.current();
    let reference = settings.retain(initial)?;
    assert_eq!(reference.generation(), initial);
    started.push((reference, snapshot("initial")));
    for ordinal in 1..65 {
        let name = ordinal.to_string();
        let candidate = settings.prepare(snapshot(&name))?;
        let key = settings.commit(candidate)?;
        let reference = settings.retain(key)?;
        assert_eq!(reference.generation(), key);
        started.push((reference, snapshot(&name)));
    }
    let current = settings.current();
    assert!(matches!(
        settings.prepare(snapshot("overflow")),
        Err(ControlError::Capacity)
    ));
    assert_eq!(settings.current(), current);
    for (reference, expected) in &started {
        assert_eq!(settings.get(reference.generation())?, expected);
    }
    let mut started = started.into_iter();
    let (reference, _) = started.next().ok_or(ControlError::Capacity)?;
    settings.release(reference)?;
    let candidate = settings.prepare(snapshot("after-release"))?;
    let replacement = settings.commit(candidate)?;
    assert_ne!(replacement, initial);
    assert_eq!(settings.get(replacement)?, &snapshot("after-release"));
    assert!(settings.get(initial).is_err());
    for (reference, expected) in started {
        assert_eq!(settings.get(reference.generation())?, &expected);
        settings.release(reference)?;
    }
    Ok(())
}

#[test]
fn sixty_four_started_generations_leave_room_for_repeated_unstarted_publications(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let mut started = Vec::with_capacity(64);
    for ordinal in 0..64 {
        let current = settings.current();
        let reference = settings.retain(current)?;
        assert_eq!(reference.generation(), current);
        started.push(reference);
        let candidate = settings.prepare(snapshot(&ordinal.to_string()))?;
        settings.commit(candidate)?;
    }
    let mut previous = settings.current();
    for ordinal in 64..192 {
        let expected = snapshot(&ordinal.to_string());
        let candidate = settings.prepare(expected)?;
        assert!(settings.get(previous).is_ok());
        let current = settings.commit(candidate)?;
        assert_ne!(current, previous);
        assert!(settings.get(previous).is_err());
        assert_eq!(settings.get(current)?, &snapshot(&ordinal.to_string()));
        previous = current;
    }
    for reference in started {
        let key = reference.generation();
        assert!(settings.get(key).is_ok());
        settings.release(reference)?;
        assert!(settings.get(key).is_err());
    }
    Ok(())
}

#[test]
fn a_reference_added_during_preparation_cannot_overwrite_a_started_snapshot(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let mut started = Vec::with_capacity(65);
    for ordinal in 0..64 {
        let current = settings.current();
        let reference = settings.retain(current)?;
        assert_eq!(reference.generation(), current);
        started.push(reference);
        let candidate = settings.prepare(snapshot(&ordinal.to_string()))?;
        settings.commit(candidate)?;
    }
    let current = settings.current();
    let candidate = settings.prepare(snapshot("replacement"))?;
    let reference = settings.retain(current)?;
    assert_eq!(reference.generation(), current);
    started.push(reference);
    assert!(matches!(
        settings.commit(candidate),
        Err(ControlError::Capacity)
    ));
    assert_eq!(settings.current(), current);
    assert_eq!(settings.get(current)?, &snapshot("63"));
    for reference in &started {
        assert!(settings.get(reference.generation()).is_ok());
    }
    settings.cancel(candidate)?;
    for reference in started {
        settings.release(reference)?;
    }
    assert_eq!(settings.get(current)?, &snapshot("63"));
    Ok(())
}

#[test]
fn reusing_storage_rejects_old_generation_keys_without_changing_the_replacement(
) -> Result<(), ControlError> {
    let mut settings = SettingsGenerations::new(snapshot("initial"), 1);
    let initial = settings.current();
    let reference = settings.retain(initial)?;
    assert_eq!(reference.generation(), initial);
    let candidate = settings.prepare(snapshot("second"))?;
    let second = settings.commit(candidate)?;
    settings.release(reference)?;
    let candidate = settings.prepare(snapshot("third"))?;
    let third = settings.commit(candidate)?;
    let reference = settings.retain(third)?;
    assert_eq!(reference.generation(), third);
    for stale in [initial, second] {
        assert!(settings.get(stale).is_err());
        assert!(settings.retain(stale).is_err());
        assert_eq!(settings.current(), third);
        assert_eq!(settings.get(third)?, &snapshot("third"));
    }
    settings.release(reference)?;
    assert_eq!(settings.get(third)?, &snapshot("third"));
    Ok(())
}
