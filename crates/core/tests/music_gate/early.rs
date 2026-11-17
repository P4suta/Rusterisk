use super::{permissions, DIRECTIONS};
use rusterisk_core::{
    AudioGate, Call, CallEvent, CallState, ConnectionClaim, CoreError, MediaConnection, MediaState,
    MuteDirection,
};

#[test]
fn early_admission_requires_ringing_and_keeps_unadmitted_media_closed() -> Result<(), CoreError> {
    for down in [Call::incoming(7), Call::outgoing(8)] {
        assert_eq!(down.state(), CallState::Down);
        assert_eq!(down.media(), MediaState::Waiting);
        assert_eq!(down.audio_gate(), AudioGate::Closed);
        assert_eq!(down.generated_audio_gate(), AudioGate::Closed);
        assert_eq!(down.admit_early_media(), Err(CoreError::NotRinging));
    }
    let ringing = Call::incoming(7).apply(CallEvent::Ring).call();
    assert_eq!(ringing.media(), MediaState::Waiting);
    assert_eq!(ringing.generated_audio_gate(), AudioGate::Closed);
    let answered = ringing.apply(CallEvent::Answer).call();
    for up in [answered, answered.hold()?] {
        assert_eq!(up.admit_early_media(), Err(CoreError::NotRinging));
    }
    let early = ringing.admit_early_media()?;
    assert_eq!(early.state(), CallState::Ringing);
    assert_eq!(early.media(), MediaState::Early);
    assert_eq!(early.generation(), ringing.generation());
    assert_eq!(early.audio_gate(), AudioGate::Closed);
    assert_eq!(early.generated_audio_gate(), AudioGate::Open);
    assert_eq!(early.media_connection(), MediaConnection::Pending);
    assert_eq!(early.connection_claim(), ConnectionClaim::Available);
    assert_eq!(early.admit_early_media(), Ok(early));
    assert_eq!(early.resume(), Err(CoreError::NotHeld));
    assert_eq!(early.hold(), Err(CoreError::NotAnswered));
    assert_eq!(early.join(11), Err(CoreError::NotAnswered));
    Ok(())
}

#[test]
fn early_audio_and_answer_preserve_local_controls_and_peer_direction() -> Result<(), CoreError> {
    let ringing = Call::incoming(7).apply(CallEvent::Ring).call();
    for (mute, local_receives, local_sends) in [
        (None, true, true),
        (Some(MuteDirection::Incoming), false, true),
        (Some(MuteDirection::Outgoing), true, false),
        (Some(MuteDirection::Both), false, false),
    ] {
        let local = match mute {
            Some(mute) => ringing.mute(mute)?,
            None => ringing,
        };
        for (direction, peer_sends, peer_receives) in DIRECTIONS {
            let configured = local.with_peer_direction(direction)?;
            assert_eq!(configured.audio_gate(), AudioGate::Closed);
            assert_eq!(configured.generated_audio_gate(), AudioGate::Closed);
            let early = configured.admit_early_media()?;
            let expected = (local_receives && peer_sends, local_sends && peer_receives);
            assert_eq!(early.state(), CallState::Ringing);
            assert_eq!(early.media(), MediaState::Early);
            assert_eq!(early.peer_direction(), direction);
            assert_eq!(early.audio_gate(), AudioGate::Closed);
            assert_eq!(permissions(early.generated_audio_gate()), expected);
            let answered = early.apply(CallEvent::Answer).call();
            assert_eq!(answered.state(), CallState::Up);
            assert_eq!(answered.media(), MediaState::Active);
            assert_eq!(answered.generation(), early.generation());
            assert_eq!(answered.peer_direction(), direction);
            assert_eq!(permissions(answered.generated_audio_gate()), expected);
            assert_eq!(permissions(answered.audio_gate()), expected);
            assert_eq!(answered.media_connection(), MediaConnection::Pending);
            let restored = early.unmute(MuteDirection::Both)?;
            assert_eq!(restored.audio_gate(), AudioGate::Closed);
            assert_eq!(restored.state(), CallState::Ringing);
            assert_eq!(restored.media(), MediaState::Early);
            assert_eq!(
                permissions(restored.generated_audio_gate()),
                (peer_sends, peer_receives)
            );
        }
    }
    Ok(())
}

#[test]
fn early_permission_and_answer_do_not_claim_or_report_a_received_media_connection(
) -> Result<(), CoreError> {
    let ringing = Call::incoming(7).apply(CallEvent::Ring).call();
    for ringing in [ringing, ringing.claim_connection()?] {
        let claim = ringing.connection_claim();
        let early = ringing.admit_early_media()?;
        assert_eq!(early.connection_claim(), claim);
        assert_eq!(early.media_connection(), MediaConnection::Pending);
        let answered_without_media = early.apply(CallEvent::Answer).call();
        assert_eq!(answered_without_media.connection_claim(), claim);
        assert_eq!(
            answered_without_media.media_connection(),
            MediaConnection::Pending
        );
        let received = early.connect_media().call();
        assert_eq!(received.state(), CallState::Ringing);
        assert_eq!(received.media(), MediaState::Early);
        assert_eq!(received.audio_gate(), AudioGate::Closed);
        assert_eq!(received.generated_audio_gate(), AudioGate::Open);
        assert_eq!(received.media_connection(), MediaConnection::Connected);
        assert_eq!(received.connection_claim(), claim);
        let answered = received.apply(CallEvent::Answer).call();
        assert_eq!(answered.state(), CallState::Up);
        assert_eq!(answered.media(), MediaState::Active);
        assert_eq!(answered.media_connection(), MediaConnection::Connected);
        assert_eq!(answered.connection_claim(), claim);
        assert_eq!(answered.generation(), ringing.generation());
    }
    Ok(())
}

#[test]
fn terminal_early_calls_reject_late_admission_and_keep_all_permissions_released(
) -> Result<(), CoreError> {
    let early = Call::incoming(7)
        .apply(CallEvent::Ring)
        .call()
        .admit_early_media()?;
    for (direction, _, _) in DIRECTIONS {
        for terminal in [CallEvent::Cancel, CallEvent::Hangup, CallEvent::Timeout] {
            let ended = early.with_peer_direction(direction)?.apply(terminal).call();
            assert_eq!(ended.state(), CallState::Ended);
            assert_eq!(ended.media(), MediaState::Released);
            assert_eq!(ended.media_connection(), MediaConnection::Released);
            assert_eq!(ended.connection_claim(), ConnectionClaim::Released);
            assert_eq!(ended.generated_audio_gate(), AudioGate::Closed);
            assert_eq!(ended.audio_gate(), AudioGate::Closed);
            assert_eq!(ended.admit_early_media(), Err(CoreError::Ended));
            for late in [CallEvent::Ring, CallEvent::Answer, terminal] {
                let unchanged = ended.apply(late).call();
                assert_eq!(unchanged, ended);
                assert_eq!(unchanged.admit_early_media(), Err(CoreError::Ended));
            }
            assert_eq!(ended.connect_media().call(), ended);
        }
    }
    Ok(())
}
