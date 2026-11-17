use rusterisk_core::{SettingsError, SettingsLifetimes};

#[test]
fn a_foreign_reference_cannot_release_a_live_local_reference() -> Result<(), SettingsError> {
    let mut first = SettingsLifetimes::new(1);
    let mut second = SettingsLifetimes::new(2);
    let foreign = first.retain(first.current())?;
    let local = second.retain(second.current())?;
    let current = local.generation();
    let local_slot = second.slot(current)?;
    let foreign_generation = foreign.generation();
    let foreign_slot = first.slot(foreign_generation)?;
    let Err(error) = second.release(foreign) else {
        return Err(SettingsError::UnknownGeneration);
    };
    assert!(matches!(&error, SettingsError::ForeignReference(_)));
    let recovered = error
        .into_reference()
        .ok_or(SettingsError::UnknownGeneration)?;
    assert_eq!(recovered.generation(), foreign_generation);
    assert_eq!(first.release(recovered)?, None);
    let first_candidate = first.prepare()?;
    let first_published = first.commit(first_candidate)?;
    assert_eq!(first_published.retired_slot(), Some(foreign_slot));
    let candidate = second.prepare()?;
    let published = second.commit(candidate)?;
    assert_eq!(published.retired_slot(), None);
    assert!(second.slot(current).is_ok());
    assert_eq!(second.release(local)?, Some(local_slot));
    assert!(second.slot(current).is_err());
    Ok(())
}

#[test]
fn a_foreign_candidate_cannot_publish_or_cancel_a_local_candidate() -> Result<(), SettingsError> {
    let mut first = SettingsLifetimes::new(1);
    let mut second = SettingsLifetimes::new(2);
    let foreign = first.prepare()?;
    let local = second.prepare()?;
    let current = second.current();
    assert!(second.commit(foreign).is_err());
    assert!(second.cancel(foreign).is_err());
    assert_eq!(second.current(), current);
    let published = second.commit(local)?;
    assert_eq!(second.current(), published.generation());
    first.cancel(foreign)?;
    Ok(())
}

#[test]
fn a_foreign_generation_cannot_select_or_retain_local_settings() -> Result<(), SettingsError> {
    let first = SettingsLifetimes::new(1);
    let mut second = SettingsLifetimes::new(2);
    assert!(second.slot(first.current()).is_err());
    assert!(second.retain(first.current()).is_err());
    assert!(second.slot(second.current()).is_ok());
    Ok(())
}
