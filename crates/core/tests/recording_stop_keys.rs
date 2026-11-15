use rusterisk_core::{DtmfDigit, RecordingStopKey};

#[test]
fn recording_stop_keys_select_only_the_requested_received_digits() -> Result<(), &'static str> {
    for event in 0..16 {
        let digit = DtmfDigit::from_event(event).ok_or("Invalid test digit")?;
        assert!(!RecordingStopKey::None.accepts(digit));
        assert!(RecordingStopKey::Any.accepts(digit));
        assert_eq!(
            RecordingStopKey::Star.accepts(digit),
            digit == DtmfDigit::Star
        );
        assert_eq!(
            RecordingStopKey::Hash.accepts(digit),
            digit == DtmfDigit::Hash
        );
    }
    Ok(())
}
