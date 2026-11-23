use rusterisk_core::{
    AnswerProgress, AnswerStatus, CallState, DialFork, DialForkError, DialForkOutcome,
    DialplanContinuation, DialplanDispatch, DialplanError, DialplanInstruction, DialplanMachine,
    DialplanToken, DtmfDigit, DtmfError, DtmfSequence, DtmfTiming, MediaConnection, Wake,
};

#[test]
fn the_earliest_wake_prefers_the_current_turn_and_then_the_sooner_instant() {
    assert_eq!(Wake::Now.earliest(Wake::At(5)), Wake::Now);
    assert_eq!(Wake::At(5).earliest(Wake::Now), Wake::Now);
    assert_eq!(Wake::Now.earliest(Wake::Idle), Wake::Now);
    assert_eq!(Wake::Idle.earliest(Wake::Now), Wake::Now);
    assert_eq!(Wake::At(5).earliest(Wake::At(3)), Wake::At(3));
    assert_eq!(Wake::At(3).earliest(Wake::At(5)), Wake::At(3));
    assert_eq!(Wake::At(5).earliest(Wake::Idle), Wake::At(5));
    assert_eq!(Wake::Idle.earliest(Wake::At(5)), Wake::At(5));
    assert_eq!(Wake::Idle.earliest(Wake::Idle), Wake::Idle);
}

fn dispatched(
    machine: &mut DialplanMachine,
    instruction: DialplanInstruction,
    next: Option<u16>,
    now: u64,
) -> Result<DialplanToken, DialplanError> {
    match machine.dispatch(instruction, next, now)? {
        DialplanDispatch::Execute(execution) => Ok(execution.token),
        DialplanDispatch::Suspended | DialplanDispatch::Yield | DialplanDispatch::Ended => {
            Err(DialplanError::Program)
        }
    }
}

#[test]
fn a_dialplan_needs_its_owner_now_at_its_wait_instant_or_after_a_completion(
) -> Result<(), DialplanError> {
    let mut machine = DialplanMachine::new(1, 0, 3)?;
    assert_eq!(machine.wake(), Wake::Now);
    let token = dispatched(&mut machine, DialplanInstruction::Stasis, Some(1), 1000)?;
    assert_eq!(machine.wake(), Wake::Idle);
    let awaiting = machine;
    machine.tick(u64::MAX);
    assert_eq!(machine, awaiting);
    assert!(machine.complete(token, DialplanContinuation::Advance));
    assert_eq!(machine.wake(), Wake::Now);
    dispatched(&mut machine, DialplanInstruction::Wait(250), Some(2), 1000)?;
    assert_eq!(machine.wake(), Wake::At(1250));
    let waiting = machine;
    machine.tick(1249);
    assert_eq!(machine, waiting);
    machine.tick(1250);
    assert_eq!(machine.wake(), Wake::Now);
    dispatched(&mut machine, DialplanInstruction::Wait(10), None, 2000)?;
    machine.tick(2010);
    assert_eq!(machine.wake(), Wake::Idle);
    Ok(())
}

#[test]
fn a_cancelled_dialplan_never_needs_its_owner() -> Result<(), DialplanError> {
    let mut machine = DialplanMachine::new(1, 0, 2)?;
    dispatched(&mut machine, DialplanInstruction::Wait(250), Some(1), 0)?;
    machine.cancel();
    assert_eq!(machine.wake(), Wake::Idle);
    Ok(())
}

fn scope() -> Result<DialplanToken, DialForkError> {
    let mut machine = DialplanMachine::new(1, 0, 1).map_err(|_| DialForkError::Stale)?;
    dispatched(&mut machine, DialplanInstruction::Dial, None, 0).map_err(|_| DialForkError::Stale)
}

#[test]
fn a_dial_fork_needs_its_owner_at_its_timeout_while_branches_are_pending(
) -> Result<(), DialForkError> {
    let scope = scope()?;
    let mut timed = DialFork::new(scope, 2, 100, Some(30_000))?;
    assert_eq!(timed.wake(), Wake::At(30_100));
    let dialing = timed;
    assert_eq!(timed.tick(30_099).outcome, DialForkOutcome::Pending);
    assert_eq!(timed, dialing);
    assert_eq!(timed.tick(30_100).outcome, DialForkOutcome::Continue);
    assert_eq!(timed.wake(), Wake::Idle);
    let mut answered = DialFork::new(scope, 2, 100, Some(30_000))?;
    answered.answer(scope, 0, 200)?;
    assert_eq!(answered.wake(), Wake::Idle);
    let untimed = DialFork::new(scope, 2, 100, None)?;
    assert_eq!(untimed.wake(), Wake::Idle);
    Ok(())
}

#[test]
fn an_answer_needs_its_owner_at_the_media_deadline_after_the_call_is_up(
) -> Result<(), DialplanError> {
    let mut answer = AnswerProgress::new();
    assert_eq!(answer.wake(), Wake::Idle);
    assert_eq!(
        answer.observe(10, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(answer.wake(), Wake::At(510));
    let waiting = answer;
    assert_eq!(
        answer.observe(509, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(answer, waiting);
    assert_eq!(
        answer.observe(510, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Ready
    );
    assert_eq!(answer.wake(), Wake::Idle);
    let mut connected = AnswerProgress::new();
    connected.observe(10, CallState::Up, MediaConnection::Connected)?;
    assert_eq!(connected.wake(), Wake::Idle);
    let mut cancelled = waiting;
    cancelled.cancel();
    assert_eq!(cancelled.wake(), Wake::Idle);
    Ok(())
}

#[test]
fn a_digit_sequence_needs_its_owner_now_until_armed_and_then_at_each_tone_instant(
) -> Result<(), DtmfError> {
    let timing = DtmfTiming::new(100, 50, 100, 80)?;
    let mut sequence = DtmfSequence::new(&[DtmfDigit::One, DtmfDigit::Hash], timing)?;
    assert_eq!(sequence.wake(), Wake::Now);
    assert!(sequence.advance(1000)?.is_none());
    assert_eq!(sequence.wake(), Wake::At(1100));
    let armed = sequence;
    assert!(sequence.advance(1099)?.is_none());
    assert_eq!(sequence, armed);
    assert!(sequence.advance(1100)?.is_some());
    assert_eq!(sequence.wake(), Wake::At(1250));
    assert!(sequence.advance(1250)?.is_some());
    assert_eq!(sequence.wake(), Wake::At(1430));
    let completing = sequence;
    assert!(sequence.advance(1429)?.is_none());
    assert_eq!(sequence, completing);
    assert!(sequence.advance(1430)?.is_none());
    assert_eq!(sequence.wake(), Wake::Idle);
    let mut immediate = DtmfSequence::new(&[DtmfDigit::One], DtmfTiming::new(0, 0, 100, 0)?)?;
    assert_eq!(immediate.wake(), Wake::Now);
    assert!(immediate.advance(7)?.is_some());
    assert_eq!(immediate.wake(), Wake::At(107));
    immediate.cancel();
    assert_eq!(immediate.wake(), Wake::Idle);
    Ok(())
}
