use rusterisk_core::{AnswerProgress, AnswerStatus, CallState, DialplanError, MediaConnection};

#[test]
fn answering_waits_for_the_response_then_media_or_five_hundred_milliseconds(
) -> Result<(), DialplanError> {
    let mut answer = AnswerProgress::new();
    assert_eq!(
        answer.observe(100, CallState::Ringing, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        answer.observe(200, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        answer.observe(699, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        answer.observe(700, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Ready
    );
    let mut media = AnswerProgress::new();
    assert_eq!(
        media.observe(100, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        media.observe(120, CallState::Up, MediaConnection::Connected)?,
        AnswerStatus::Ready
    );
    assert_eq!(
        media.observe(121, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Ready
    );
    Ok(())
}

#[test]
fn cancellation_is_terminal_and_clock_failure_preserves_the_pending_phase(
) -> Result<(), DialplanError> {
    for state in [
        CallState::Down,
        CallState::Ringing,
        CallState::Up,
        CallState::Ended,
    ] {
        let mut answer = AnswerProgress::new();
        answer.cancel();
        assert_eq!(
            answer.observe(1000, state, MediaConnection::Connected)?,
            AnswerStatus::Cancelled
        );
    }
    let mut answer = AnswerProgress::new();
    assert_eq!(
        answer.observe(u64::MAX, CallState::Up, MediaConnection::Pending),
        Err(DialplanError::Clock)
    );
    assert_eq!(
        answer.observe(0, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        answer.observe(499, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Pending
    );
    assert_eq!(
        answer.observe(500, CallState::Up, MediaConnection::Pending)?,
        AnswerStatus::Ready
    );
    let mut ended = AnswerProgress::new();
    assert_eq!(
        ended.observe(0, CallState::Ended, MediaConnection::Released)?,
        AnswerStatus::Cancelled
    );
    assert_eq!(
        ended.observe(1, CallState::Up, MediaConnection::Connected)?,
        AnswerStatus::Cancelled
    );
    Ok(())
}
