use rusterisk_core::{RecordingGate, RecordingSilence};

#[test]
fn silence_limits_reset_after_voice_and_stop_only_after_the_configured_run() {
    let mut silence = RecordingSilence::new(1);
    assert!(!silence.observe(1000, true, RecordingGate::Open));
    assert!(!silence.observe(20, false, RecordingGate::Open));
    assert_eq!(silence.trailing_milliseconds(), 0);
    assert!(!silence.observe(1000, true, RecordingGate::Silent));
    assert!(silence.observe(20, true, RecordingGate::Silent));
    assert_eq!(silence.trailing_milliseconds(), 1020);
    assert!(silence.observe(20, false, RecordingGate::Open));
    assert_eq!(silence.trailing_milliseconds(), 1020);
    let mut disabled = RecordingSilence::new(0);
    assert!(!disabled.observe(u64::MAX, true, RecordingGate::Open));
    assert_eq!(disabled.trailing_milliseconds(), 0);
}

#[test]
fn resumed_silence_excludes_the_paused_prefix_and_a_long_paused_run_matches_the_reference_cutoff() {
    let mut silence = RecordingSilence::new(1);
    assert!(!silence.observe(800, true, RecordingGate::Open));
    assert!(!silence.observe(200, true, RecordingGate::Paused));
    assert!(!silence.observe(20, true, RecordingGate::Open));
    assert_eq!(silence.trailing_milliseconds(), 20);
    assert!(!silence.observe(980, true, RecordingGate::Open));
    assert!(silence.observe(20, true, RecordingGate::Open));
    let mut paused = RecordingSilence::new(1);
    assert!(paused.observe(1020, true, RecordingGate::PausedSilent));
    let mut overflow = RecordingSilence::new(u32::MAX);
    assert!(overflow.observe(u64::MAX, true, RecordingGate::Open));
}
