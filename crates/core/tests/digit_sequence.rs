use rusterisk_core::{DtmfDigit, DtmfError, DtmfSequence, DtmfTiming};

#[test]
fn digit_timing_uses_actual_start_times_and_cancellation_is_terminal() -> Result<(), DtmfError> {
    let timing = DtmfTiming::new(100, 50, 100, 80)?;
    let mut sequence = DtmfSequence::new(&[DtmfDigit::One, DtmfDigit::Hash], timing)?;
    assert!(sequence.advance(0)?.is_none());
    assert!(sequence.advance(99)?.is_none());
    let first = sequence.advance(100)?.ok_or(DtmfError::Empty)?;
    assert_eq!(first.digit, DtmfDigit::One);
    assert_eq!(first.duration_ms, 100);
    assert!(sequence.advance(249)?.is_none());
    let last = sequence.advance(500)?.ok_or(DtmfError::Empty)?;
    assert_eq!(last.digit, DtmfDigit::Hash);
    assert!(!sequence.finished());
    assert!(sequence.advance(679)?.is_none());
    assert!(sequence.advance(680)?.is_none());
    assert!(sequence.finished());
    assert!(sequence.advance(u64::MAX)?.is_none());
    let mut cancelled = DtmfSequence::new(
        &[DtmfDigit::One, DtmfDigit::Two],
        DtmfTiming::new(0, 0, 100, 0)?,
    )?;
    assert!(cancelled.advance(0)?.is_some());
    cancelled.cancel();
    assert!(cancelled.finished());
    assert!(cancelled.advance(1000)?.is_none());
    Ok(())
}

#[test]
fn digit_sequence_bounds_and_clock_overflow_fail_before_mutating_state() -> Result<(), DtmfError> {
    let timing = DtmfTiming::new(100, 0, 100, 0)?;
    assert!(matches!(
        DtmfSequence::new(&[], timing),
        Err(DtmfError::Empty)
    ));
    assert!(matches!(
        DtmfSequence::new(&[DtmfDigit::Zero; 33], timing),
        Err(DtmfError::Capacity)
    ));
    for values in [
        (0, 0, 0, 0),
        (5001, 0, 100, 0),
        (0, 5001, 100, 0),
        (0, 0, 5001, 0),
        (0, 0, 100, 5001),
    ] {
        assert_eq!(
            DtmfTiming::new(values.0, values.1, values.2, values.3),
            Err(DtmfError::Timing)
        );
    }
    let mut sequence = DtmfSequence::new(&[DtmfDigit::One], timing)?;
    assert_eq!(sequence.advance(u64::MAX), Err(DtmfError::Clock));
    assert!(sequence.advance(0)?.is_none());
    assert!(sequence.advance(100)?.is_some());
    Ok(())
}
