use rusterisk_core::{Call, CallEvent, CoreError, StasisApplication, StasisMembership};

#[test]
fn incoming_calls_can_enter_stasis_before_answer_and_outgoing_reservations_wait_for_answer(
) -> Result<(), CoreError> {
    let app = StasisApplication::new(1);
    let incoming = Call::incoming(1).apply(CallEvent::Ring).call();
    assert_eq!(incoming.stasis(), StasisMembership::Outside);
    let entered = incoming.enter_stasis(app)?;
    assert_eq!(entered.stasis(), StasisMembership::Entered(app));
    assert_eq!(entered.enter_stasis(app)?, entered);
    let pending = Call::outgoing(2).reserve_stasis(app)?;
    assert_eq!(pending.stasis(), StasisMembership::Pending(app));
    assert_eq!(pending.enter_stasis(app), Err(CoreError::NotAnswered));
    assert_eq!(
        pending
            .apply(CallEvent::Answer)
            .call()
            .enter_stasis(app)?
            .stasis(),
        StasisMembership::Entered(app)
    );
    Ok(())
}

#[test]
fn stasis_ownership_changes_require_the_current_app_and_termination_releases_membership(
) -> Result<(), CoreError> {
    let app = StasisApplication::new(1);
    let other = StasisApplication::new(2);
    let entered = Call::incoming(1).enter_stasis(app)?;
    assert_eq!(entered.enter_stasis(other), Err(CoreError::StasisConflict));
    assert_eq!(
        entered.leave_stasis(other),
        Err(CoreError::WrongStasisApplication)
    );
    let moved = entered.move_stasis(app, other)?;
    assert_eq!(moved.stasis(), StasisMembership::Entered(other));
    assert_eq!(
        moved.leave_stasis(other)?.stasis(),
        StasisMembership::Outside
    );
    for event in [CallEvent::Cancel, CallEvent::Hangup, CallEvent::Timeout] {
        let ended = moved.apply(event).call();
        assert_eq!(ended.stasis(), StasisMembership::Released);
        assert_eq!(ended.enter_stasis(app), Err(CoreError::Ended));
        assert_eq!(ended.reserve_stasis(app), Err(CoreError::Ended));
        assert_eq!(ended.move_stasis(other, app), Err(CoreError::Ended));
        assert_eq!(ended.leave_stasis(other), Err(CoreError::Ended));
        assert_eq!(ended.apply(CallEvent::Answer).call(), ended);
    }
    Ok(())
}
