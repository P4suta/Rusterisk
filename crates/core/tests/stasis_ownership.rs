use rusterisk_core::{Call, CallEvent, CoreError, StasisApplication, StasisMembership};

const APPLICATION_PAIRS: [(u64, u64); 3] = [(1, 2), (7, 7 | (1 << 63)), (u64::MAX, 0)];

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

#[test]
fn stasis_reservations_preserve_membership_only_for_the_current_application(
) -> Result<(), CoreError> {
    for (application, other) in APPLICATION_PAIRS {
        let application = StasisApplication::new(application);
        let other = StasisApplication::new(other);
        for call in [
            Call::outgoing(1).reserve_stasis(application)?,
            Call::incoming(2).enter_stasis(application)?,
        ] {
            assert_eq!(call.reserve_stasis(application)?, call);
            assert_eq!(call.reserve_stasis(other), Err(CoreError::StasisConflict));
        }
    }
    Ok(())
}

#[test]
fn answered_stasis_reservations_admit_only_the_reserved_application() -> Result<(), CoreError> {
    for (application, other) in APPLICATION_PAIRS {
        let application = StasisApplication::new(application);
        let other = StasisApplication::new(other);
        let pending = Call::outgoing(1)
            .reserve_stasis(application)?
            .apply(CallEvent::Answer)
            .call();
        assert_eq!(pending.enter_stasis(other), Err(CoreError::StasisConflict));
        let entered = pending.enter_stasis(application)?;
        assert_eq!(entered.stasis(), StasisMembership::Entered(application));
        assert_eq!(entered.enter_stasis(application)?, entered);
        assert_eq!(entered.enter_stasis(other), Err(CoreError::StasisConflict));
    }
    Ok(())
}

#[test]
fn stasis_moves_require_the_current_application_as_the_source() -> Result<(), CoreError> {
    for (application, other) in APPLICATION_PAIRS {
        let application = StasisApplication::new(application);
        let other = StasisApplication::new(other);
        let entered = Call::incoming(1).enter_stasis(application)?;
        assert_eq!(
            entered.move_stasis(other, other),
            Err(CoreError::WrongStasisApplication)
        );
        let moved = entered.move_stasis(application, other)?;
        assert_eq!(moved.stasis(), StasisMembership::Entered(other));
        assert_eq!(moved.move_stasis(other, other)?, moved);
        assert_eq!(
            moved.move_stasis(application, application),
            Err(CoreError::WrongStasisApplication)
        );
        assert_eq!(
            moved.move_stasis(other, application)?.stasis(),
            StasisMembership::Entered(application)
        );
    }
    Ok(())
}
