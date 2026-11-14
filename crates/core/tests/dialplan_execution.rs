use rusterisk_core::{
    DialplanContinuation, DialplanDispatch, DialplanError, DialplanInstruction, DialplanMachine,
    DialplanState,
};

#[test]
fn resumed_positions_preserve_tokens_and_cannot_revive_cancelled_execution(
) -> Result<(), DialplanError> {
    let mut machine = DialplanMachine::new(1, 0, 4)?;
    let DialplanDispatch::Execute(first) =
        machine.dispatch(DialplanInstruction::Stasis, Some(1), 0)?
    else {
        return Err(DialplanError::Program);
    };
    let before = machine;
    assert!(!machine.complete(first.token, DialplanContinuation::Jump(4)));
    assert_eq!(machine, before);
    assert!(machine.complete(first.token, DialplanContinuation::Jump(3)));
    assert_eq!(machine.state(), DialplanState::Ready { position: 3 });
    let DialplanDispatch::Execute(second) =
        machine.dispatch(DialplanInstruction::Stasis, None, 0)?
    else {
        return Err(DialplanError::Program);
    };
    assert_ne!(first.token, second.token);
    assert!(!machine.complete(first.token, DialplanContinuation::Jump(0)));
    machine.cancel();
    assert!(!machine.complete(second.token, DialplanContinuation::Jump(0)));
    assert_eq!(machine.state(), DialplanState::Ended);
    Ok(())
}

#[test]
fn operation_tokens_from_another_call_cannot_advance_execution() -> Result<(), DialplanError> {
    let mut first = DialplanMachine::new(7, 0, 1)?;
    let mut second = DialplanMachine::new(8, 0, 1)?;
    let DialplanDispatch::Execute(first_step) =
        first.dispatch(DialplanInstruction::Answer, None, 0)?
    else {
        return Err(DialplanError::Program);
    };
    let DialplanDispatch::Execute(second_step) =
        second.dispatch(DialplanInstruction::Answer, None, 0)?
    else {
        return Err(DialplanError::Program);
    };
    let before = second;
    assert!(!second.complete(first_step.token, DialplanContinuation::Advance));
    assert_eq!(second, before);
    assert!(second.complete(second_step.token, DialplanContinuation::Advance));
    Ok(())
}

#[test]
fn asynchronous_operations_suspend_until_their_own_token_completes() -> Result<(), DialplanError> {
    let mut machine = DialplanMachine::new(1, 0, 3)?;
    let DialplanDispatch::Execute(first) =
        machine.dispatch(DialplanInstruction::Set, Some(1), 0)?
    else {
        return Err(DialplanError::Program);
    };
    assert_eq!(
        machine.state(),
        DialplanState::Awaiting {
            position: 0,
            token: first.token
        }
    );
    assert_eq!(
        machine.dispatch(DialplanInstruction::Answer, Some(2), 0)?,
        DialplanDispatch::Suspended
    );
    assert!(machine.complete(first.token, DialplanContinuation::Advance));
    let DialplanDispatch::Execute(second) =
        machine.dispatch(DialplanInstruction::Answer, Some(2), 0)?
    else {
        return Err(DialplanError::Program);
    };
    assert_ne!(first.token, second.token);
    assert!(!machine.complete(first.token, DialplanContinuation::End));
    assert_eq!(
        machine.state(),
        DialplanState::Awaiting {
            position: 1,
            token: second.token
        }
    );
    assert!(machine.complete(second.token, DialplanContinuation::Advance));
    assert_eq!(machine.state(), DialplanState::Ready { position: 2 });
    Ok(())
}

#[test]
fn wait_deadlines_and_explicit_jump_targets_preserve_the_execution_location(
) -> Result<(), DialplanError> {
    let mut machine = DialplanMachine::new(1, 0, 4)?;
    let DialplanDispatch::Execute(wait) =
        machine.dispatch(DialplanInstruction::Wait(100), Some(3), 20)?
    else {
        return Err(DialplanError::Program);
    };
    assert!(!machine.complete(wait.token, DialplanContinuation::Advance));
    machine.tick(119);
    assert_eq!(
        machine.state(),
        DialplanState::Waiting {
            position: 0,
            deadline: 120
        }
    );
    machine.tick(120);
    assert_eq!(machine.state(), DialplanState::Ready { position: 3 });
    let DialplanDispatch::Execute(jump) =
        machine.dispatch(DialplanInstruction::Goto, Some(1), 120)?
    else {
        return Err(DialplanError::Program);
    };
    assert!(machine.complete(jump.token, DialplanContinuation::Advance));
    assert_eq!(machine.state(), DialplanState::Ready { position: 1 });
    let DialplanDispatch::Execute(last) = machine.dispatch(DialplanInstruction::NoOp, None, 120)?
    else {
        return Err(DialplanError::Program);
    };
    assert!(machine.complete(last.token, DialplanContinuation::Advance));
    assert_eq!(machine.state(), DialplanState::Ended);
    Ok(())
}

#[test]
fn every_instruction_stays_terminal_after_cancellation() -> Result<(), DialplanError> {
    for instruction in [
        DialplanInstruction::NoOp,
        DialplanInstruction::Set,
        DialplanInstruction::Answer,
        DialplanInstruction::Dial,
        DialplanInstruction::Stasis,
        DialplanInstruction::Playback,
        DialplanInstruction::Wait(100),
        DialplanInstruction::Goto,
        DialplanInstruction::Hangup,
    ] {
        let mut machine = DialplanMachine::new(1, 0, 2)?;
        let DialplanDispatch::Execute(execution) = machine.dispatch(instruction, Some(1), 0)?
        else {
            return Err(DialplanError::Program);
        };
        machine.cancel();
        let terminal = machine;
        machine.begin_turn();
        machine.tick(u64::MAX);
        assert!(!machine.complete(execution.token, DialplanContinuation::Advance));
        assert_eq!(
            machine.dispatch(DialplanInstruction::NoOp, Some(0), u64::MAX)?,
            DialplanDispatch::Ended
        );
        assert_eq!(machine.state(), DialplanState::Ended);
        assert_eq!(machine, terminal);
    }
    Ok(())
}

#[test]
fn tight_loops_yield_and_invalid_targets_or_clocks_leave_state_unchanged(
) -> Result<(), DialplanError> {
    assert_eq!(DialplanMachine::new(1, 0, 0), Err(DialplanError::Program));
    assert_eq!(
        DialplanMachine::new(1, 4096, 4096),
        Err(DialplanError::Program)
    );
    assert_eq!(
        DialplanMachine::new(1, 0, 4097),
        Err(DialplanError::Program)
    );
    let mut machine = DialplanMachine::new(1, 0, 1)?;
    let initial = machine;
    assert_eq!(
        machine.dispatch(DialplanInstruction::NoOp, Some(1), 0),
        Err(DialplanError::Program)
    );
    assert_eq!(machine, initial);
    assert_eq!(
        machine.dispatch(DialplanInstruction::Goto, None, 0),
        Err(DialplanError::Program)
    );
    assert_eq!(machine, initial);
    assert_eq!(
        machine.dispatch(DialplanInstruction::Wait(1), Some(0), u64::MAX),
        Err(DialplanError::Clock)
    );
    assert_eq!(machine, initial);
    for _ in 0..32 {
        let DialplanDispatch::Execute(execution) =
            machine.dispatch(DialplanInstruction::Goto, Some(0), 0)?
        else {
            return Err(DialplanError::Program);
        };
        assert!(machine.complete(execution.token, DialplanContinuation::Advance));
    }
    assert_eq!(
        machine.dispatch(DialplanInstruction::NoOp, Some(0), 0)?,
        DialplanDispatch::Yield
    );
    machine.begin_turn();
    assert!(matches!(
        machine.dispatch(DialplanInstruction::NoOp, Some(0), 0)?,
        DialplanDispatch::Execute(_)
    ));
    Ok(())
}
