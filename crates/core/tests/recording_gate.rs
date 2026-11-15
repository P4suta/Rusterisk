use rusterisk_core::{RecordingControl, RecordingGate};

#[test]
fn pause_and_input_controls_are_independent_and_idempotent_in_every_state() {
    for state in [
        RecordingGate::Open,
        RecordingGate::Silent,
        RecordingGate::Paused,
        RecordingGate::PausedSilent,
    ] {
        for operation in [
            RecordingControl::Pause,
            RecordingControl::Resume,
            RecordingControl::Mute,
            RecordingControl::Unmute,
        ] {
            let changed = state.apply(operation);
            assert_eq!(changed.apply(operation), changed);
        }
        for transport in [RecordingControl::Pause, RecordingControl::Resume] {
            for input in [RecordingControl::Mute, RecordingControl::Unmute] {
                assert_eq!(
                    state.apply(transport).apply(input),
                    state.apply(input).apply(transport)
                );
            }
        }
    }
}

#[test]
fn pausing_and_resuming_preserve_the_selected_input_without_releasing_recording_ownership() {
    let state = RecordingGate::Open.apply(RecordingControl::Mute);
    assert_eq!(state, RecordingGate::Silent);
    let state = state.apply(RecordingControl::Pause);
    assert_eq!(state, RecordingGate::PausedSilent);
    assert_eq!(state.apply(RecordingControl::Resume), RecordingGate::Silent);
    assert_eq!(state.apply(RecordingControl::Unmute), RecordingGate::Paused);
    assert_eq!(
        state
            .apply(RecordingControl::Unmute)
            .apply(RecordingControl::Resume),
        RecordingGate::Open
    );
}
