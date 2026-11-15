use rusterisk_core::{AudioGate, Call, CallEvent, CoreError, MuteDirection, PeerMediaDirection};

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
fn peer_media_permissions_intersect_local_controls_without_replacing_them() -> Result<(), CoreError>
{
    let answered = Call::incoming(7).apply(CallEvent::Answer).call();
    for local in [
        None,
        Some(MuteDirection::Incoming),
        Some(MuteDirection::Outgoing),
        Some(MuteDirection::Both),
    ] {
        let call = match local {
            None => answered,
            Some(direction) => answered.mute(direction)?,
        };
        let (local_receive, local_send) = permissions(call.audio_gate());
        for (direction, peer_sends, peer_receives) in DIRECTIONS {
            let negotiated = call.with_peer_direction(direction)?;
            assert_eq!(negotiated.peer_direction(), direction);
            assert_eq!(direction.can_send(), peer_sends);
            assert_eq!(direction.can_receive(), peer_receives);
            assert_eq!(
                permissions(negotiated.audio_gate()),
                (local_receive && peer_sends, local_send && peer_receives)
            );
            assert_eq!(negotiated.with_peer_direction(direction)?, negotiated);
            assert_eq!(
                negotiated
                    .with_peer_direction(PeerMediaDirection::SendReceive)?
                    .audio_gate(),
                call.audio_gate()
            );
            let held = call.hold()?.with_peer_direction(direction)?;
            assert_eq!(held.audio_gate(), AudioGate::Closed);
            assert_eq!(held.resume()?.audio_gate(), negotiated.audio_gate());
            let restored = negotiated.unmute(MuteDirection::Both)?;
            assert_eq!(
                permissions(restored.audio_gate()),
                (peer_sends, peer_receives)
            );
        }
    }
    Ok(())
}

#[test]
fn media_direction_updates_cannot_activate_unanswered_or_terminal_calls() -> Result<(), CoreError> {
    let initial = Call::incoming(7);
    assert_eq!(initial.peer_direction(), PeerMediaDirection::SendReceive);
    for (direction, _, _) in DIRECTIONS {
        let pending = initial.with_peer_direction(direction)?;
        assert_eq!(pending.audio_gate(), AudioGate::Closed);
        for event in [CallEvent::Cancel, CallEvent::Hangup, CallEvent::Timeout] {
            let ended = pending.apply(event).call();
            assert_eq!(ended.audio_gate(), AudioGate::Closed);
            for (late, _, _) in DIRECTIONS {
                assert_eq!(ended.with_peer_direction(late), Err(CoreError::Ended));
                assert_eq!(
                    ended.apply(CallEvent::Answer).call().audio_gate(),
                    AudioGate::Closed
                );
            }
        }
    }
    Ok(())
}
