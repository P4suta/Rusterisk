use rusterisk_core::{
    DialFork, DialForkError, DialForkOutcome, DialForkState, DialplanDispatch, DialplanError,
    DialplanInstruction, DialplanMachine, DialplanToken,
};

#[derive(Debug)]
enum TestError {
    Plan(DialplanError),
    Fork(DialForkError),
}
impl From<DialplanError> for TestError {
    fn from(error: DialplanError) -> Self {
        Self::Plan(error)
    }
}
impl From<DialForkError> for TestError {
    fn from(error: DialForkError) -> Self {
        Self::Fork(error)
    }
}
impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan(error) => write!(f, "plan: {error:?}"),
            Self::Fork(error) => write!(f, "fork: {error:?}"),
        }
    }
}
fn token(generation: u64) -> Result<DialplanToken, TestError> {
    let mut machine = DialplanMachine::new(generation, 0, 1)?;
    let DialplanDispatch::Execute(execution) =
        machine.dispatch(DialplanInstruction::Dial, None, 0)?
    else {
        return Err(TestError::Plan(DialplanError::Program));
    };
    Ok(execution.token)
}

#[test]
fn the_first_answer_selects_one_winner_and_releases_every_other_branch() -> Result<(), TestError> {
    let token = token(1)?;
    for count in 1..=16 {
        for winner in 0..count {
            let mut fork = DialFork::new(token, count, 0, Some(100))?;
            let decision = fork.answer(token, winner, 1)?;
            assert_eq!(decision.outcome, DialForkOutcome::Connect { winner });
            let all = u16::MAX >> (16 - count);
            assert_eq!(decision.release, all & !(1 << winner));
            assert_eq!(fork.state(), DialForkState::Bridged { winner });
            assert_eq!(fork.answer(token, winner, 2)?.release, 0);
            for other in 0..count {
                if other != winner {
                    let late = fork.answer(token, other, 2)?;
                    assert_eq!(late.outcome, DialForkOutcome::Pending);
                    assert_eq!(late.release, 1 << other);
                    assert_eq!(fork.state(), DialForkState::Bridged { winner });
                }
            }
            let ended = fork.failed(token, winner, 3)?;
            assert_eq!(ended.outcome, DialForkOutcome::End);
            assert_eq!(ended.release, 1 << winner);
            let terminal = fork;
            fork.answer(token, winner, 4)?;
            fork.tick(u64::MAX);
            fork.cancel();
            assert_eq!(fork, terminal);
        }
    }
    Ok(())
}

#[test]
fn failures_and_timeouts_resume_the_dialplan_once_and_failed_branches_cannot_win(
) -> Result<(), TestError> {
    let token = token(1)?;
    let mut fork = DialFork::new(token, 2, 10, Some(100))?;
    assert_eq!(fork.failed(token, 0, 11)?.outcome, DialForkOutcome::Pending);
    assert_eq!(fork.answer(token, 0, 12)?.release, 1);
    assert_eq!(fork.state(), DialForkState::Dialing { pending: 2 });
    assert_eq!(fork.tick(109).outcome, DialForkOutcome::Pending);
    let timeout = fork.tick(110);
    assert_eq!(timeout.release, 2);
    assert_eq!(timeout.outcome, DialForkOutcome::Continue);
    assert_eq!(fork.tick(111).outcome, DialForkOutcome::Pending);
    assert_eq!(fork.answer(token, 1, 111)?.release, 2);
    assert_eq!(fork.state(), DialForkState::Ended);
    let mut all_failed = DialFork::new(token, 2, 0, None)?;
    all_failed.failed(token, 1, 0)?;
    assert_eq!(
        all_failed.failed(token, 0, 1)?.outcome,
        DialForkOutcome::Continue
    );
    assert_eq!(
        all_failed.failed(token, 0, 2)?.outcome,
        DialForkOutcome::Pending
    );
    Ok(())
}

#[test]
fn foreign_tokens_and_invalid_branches_leave_ownership_unchanged() -> Result<(), TestError> {
    let own = token(1)?;
    let foreign = token(2)?;
    assert_eq!(DialFork::new(own, 0, 0, None), Err(DialForkError::Capacity));
    assert_eq!(
        DialFork::new(own, 17, 0, None),
        Err(DialForkError::Capacity)
    );
    assert_eq!(
        DialFork::new(own, 1, u64::MAX, Some(1)),
        Err(DialForkError::Clock)
    );
    let mut fork = DialFork::new(own, 2, 0, None)?;
    let before = fork;
    assert_eq!(fork.answer(foreign, 0, 0), Err(DialForkError::Stale));
    assert_eq!(fork.failed(foreign, 0, 0), Err(DialForkError::Stale));
    assert_eq!(fork.answer(own, 2, 0), Err(DialForkError::Branch));
    assert_eq!(fork, before);
    let cancelled = fork.cancel();
    assert_eq!(cancelled.release, 3);
    assert_eq!(cancelled.outcome, DialForkOutcome::End);
    let terminal = fork;
    fork.answer(own, 0, u64::MAX)?;
    assert_eq!(fork, terminal);
    Ok(())
}
