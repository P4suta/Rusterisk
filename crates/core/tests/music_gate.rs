use rusterisk_core::{
    AudioGate, Call, CallEvent, CoreError, MediaState, MuteDirection, PeerMediaDirection,
};

const DIRECTIONS: [(PeerMediaDirection, bool, bool); 4] = [
    (PeerMediaDirection::SendReceive, true, true),
    (PeerMediaDirection::SendOnly, true, false),
    (PeerMediaDirection::ReceiveOnly, false, true),
    (PeerMediaDirection::Inactive, false, false),
];

fn permissions(gate: AudioGate) -> (bool, bool) {
    match gate {
        AudioGate::Open => (true, true),
        AudioGate::IncomingMuted => (false, true),
        AudioGate::OutgoingMuted => (true, false),
        AudioGate::Closed => (false, false),
    }
}

#[test]
fn music_permission_preserves_mute_direction_and_hold_without_opening_conversation_audio(
) -> Result<(), CoreError> {
    let answered = Call::incoming(7).apply(CallEvent::Answer).call();
    for (mute, local_receives, local_sends) in [
        (None, true, true),
        (Some(MuteDirection::Incoming), false, true),
        (Some(MuteDirection::Outgoing), true, false),
        (Some(MuteDirection::Both), false, false),
    ] {
        let local = match mute {
            Some(mute) => answered.mute(mute)?,
            None => answered,
        };
        for (direction, peer_sends, peer_receives) in DIRECTIONS {
            let active = local.with_peer_direction(direction)?;
            let held = active.hold()?;
            let expected = (local_receives && peer_sends, local_sends && peer_receives);
            assert_eq!(permissions(active.audio_gate()), expected);
            assert_eq!(permissions(active.music_gate()), expected);
            assert_eq!(permissions(held.music_gate()), expected);
            assert_eq!(held.media(), MediaState::Held);
            assert_eq!(held.audio_gate(), AudioGate::Closed);
            assert_eq!(held.generation(), active.generation());
            assert_eq!(held.resume()?, active);
            let restored = held.unmute(MuteDirection::Both)?;
            assert_eq!(
                permissions(restored.music_gate()),
                (peer_sends, peer_receives)
            );
            assert_eq!(restored.audio_gate(), AudioGate::Closed);
            assert_eq!(restored.media(), MediaState::Held);
        }
    }
    Ok(())
}

#[test]
fn music_and_conversation_permissions_stay_closed_after_every_terminal_event(
) -> Result<(), CoreError> {
    let pending = Call::incoming(7);
    let ringing = pending.apply(CallEvent::Ring).call();
    let answered = pending.apply(CallEvent::Answer).call();
    for direction in DIRECTIONS.map(|(direction, _, _)| direction) {
        for call in [pending, ringing, answered, answered.hold()?] {
            for event in [CallEvent::Cancel, CallEvent::Hangup, CallEvent::Timeout] {
                let ended = call.with_peer_direction(direction)?.apply(event).call();
                assert_eq!(ended.audio_gate(), AudioGate::Closed);
                assert_eq!(ended.music_gate(), AudioGate::Closed);
                assert_eq!(ended.generation(), call.generation());
                for late in [CallEvent::Answer, CallEvent::Ring, event] {
                    let late = ended.apply(late).call();
                    assert_eq!(late.audio_gate(), AudioGate::Closed);
                    assert_eq!(late.music_gate(), AudioGate::Closed);
                }
                assert_eq!(ended.with_peer_direction(direction), Err(CoreError::Ended));
                assert_eq!(ended.unmute(MuteDirection::Both), Err(CoreError::Ended));
                assert_eq!(ended.resume(), Err(CoreError::Ended));
            }
        }
    }
    Ok(())
}
