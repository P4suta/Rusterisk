use rusterisk_core::{CallHandle, CallTable, TeardownStep};

#[test]
fn only_a_completed_teardown_and_matching_media_acknowledgement_release_a_slot(
) -> Result<(), &'static str> {
    let mut table = CallTable::<1>::new();
    let first = CallHandle::new(1);
    let second = CallHandle::new(65);
    assert!(!table.admit(CallHandle::new(0)));
    assert!(!table.admit(CallHandle::new(2)));
    assert!(table.available(first));
    assert!(table.admit(first));
    assert!(!table.admit(first));
    assert!(!table.acknowledge(first));
    assert!(table.teardown(second).is_none());
    let mut cleanup = table.teardown(first).ok_or("missing teardown")?;
    assert!(table.teardown(first).is_none());
    assert!(!table.available(second));
    for step in [
        TeardownStep::Admissions,
        TeardownStep::Participants,
        TeardownStep::Recordings,
        TeardownStep::Dial,
        TeardownStep::Bridge,
        TeardownStep::Media,
    ] {
        assert!(!table.acknowledge(first));
        assert_eq!(cleanup.next(), Some(step));
    }
    assert_eq!(cleanup.next(), None);
    assert!(table.await_media(cleanup));
    assert!(!table.acknowledge(second));
    assert!(!table.admit(second));
    assert!(table.acknowledge(first));
    assert!(!table.acknowledge(first));
    assert!(!table.available(first));
    assert!(table.admit(second));
    assert!(table.teardown(first).is_none());
    assert!(!table.acknowledge(first));
    assert!(!table.available(CallHandle::new(129)));
    Ok(())
}

#[test]
fn partial_teardown_cannot_authorize_slot_reuse() -> Result<(), &'static str> {
    let mut table = CallTable::<1>::new();
    let key = CallHandle::new(1);
    assert!(table.admit(key));
    let mut cleanup = table.teardown(key).ok_or("missing teardown")?;
    assert_eq!(cleanup.next(), Some(TeardownStep::Admissions));
    assert!(!table.await_media(cleanup));
    assert!(!table.acknowledge(key));
    assert!(!table.available(CallHandle::new(65)));
    Ok(())
}
