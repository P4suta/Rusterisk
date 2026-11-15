use rusterisk_core::{RecordingClock, RecordingGate};

#[test]
fn duration_limits_use_complete_active_seconds_and_expiry_is_absorbing() {
    let mut clock = RecordingClock::new(1);
    assert!(!clock.advance(1999, RecordingGate::Open));
    assert!(!clock.advance(100_000, RecordingGate::Paused));
    assert!(!clock.advance(100_000, RecordingGate::PausedSilent));
    assert!(clock.advance(1, RecordingGate::Silent));
    assert!(clock.advance(0, RecordingGate::Paused));
    assert!(clock.advance(u64::MAX, RecordingGate::Open));
    let mut unlimited = RecordingClock::new(0);
    for _ in 0..4 {
        assert!(!unlimited.advance(u64::MAX, RecordingGate::Open));
    }
    let mut largest = RecordingClock::new(u32::MAX);
    assert!(!largest.advance(u64::from(u32::MAX) * 1000 + 999, RecordingGate::Open));
    assert!(largest.advance(u64::MAX, RecordingGate::Open));
}

#[test]
fn splitting_active_time_into_ticks_preserves_the_expiration_decision() {
    for limit in 1..5 {
        for first in [0, 1, 999, 1000, 1999, 5000] {
            for second in [0, 1, 999, 1000, 1999, 5000] {
                let mut together = RecordingClock::new(limit);
                let mut split = RecordingClock::new(limit);
                let expired = together.advance(first + second, RecordingGate::Open);
                split.advance(first, RecordingGate::Silent);
                assert_eq!(expired, split.advance(second, RecordingGate::Open));
            }
        }
    }
}
