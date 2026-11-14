use rusterisk_core::{Call, CallEvent, CallState, CoreError, MediaState, Transition};

#[test]
fn cancellation_wins_over_a_late_answer() {
    let call = Call::outgoing(7);
    let cancelled = call.apply(CallEvent::Cancel);
    assert_eq!(cancelled.state(), CallState::Ended);
    assert_eq!(cancelled.transition(), Transition::Release);
    let late = cancelled.call().apply(CallEvent::Answer);
    assert_eq!(late.state(), CallState::Ended);
    assert_eq!(late.transition(), Transition::Ignore);
    assert_eq!(late.call().bridge(), None);
}

#[test]
fn hold_requires_an_answer_and_resume_requires_hold() {
    let call = Call::incoming(3);
    assert_eq!(call.hold(), Err(CoreError::NotAnswered));
    let answered = call.apply(CallEvent::Answer).call();
    let held = answered.hold();
    assert!(held.is_ok());
    if let Ok(call) = held {
        assert_eq!(call.media(), MediaState::Held);
        assert_eq!(
            call.resume().map(|value| value.media()),
            Ok(MediaState::Active)
        );
    }
}

#[test]
fn terminal_states_release_owned_resources_for_every_event() {
    let events = [
        CallEvent::Ring,
        CallEvent::Answer,
        CallEvent::Cancel,
        CallEvent::Hangup,
        CallEvent::Timeout,
    ];
    for first in events {
        for second in events {
            for third in events {
                let mut call = Call::outgoing(9);
                let mut ended = false;
                for event in [first, second, third] {
                    call = call.apply(event).call();
                    if ended {
                        assert_eq!(call.state(), CallState::Ended);
                        assert_eq!(call.bridge(), None);
                        assert_eq!(call.media(), MediaState::Released);
                    }
                    ended = call.state() == CallState::Ended;
                }
            }
        }
    }
}

#[test]
fn a_channel_can_only_belong_to_one_bridge() {
    let call = Call::incoming(4).apply(CallEvent::Answer).call();
    let joined = call.join(11);
    assert!(joined.is_ok());
    if let Ok(call) = joined {
        assert_eq!(call.join(12), Err(CoreError::AlreadyBridged));
        assert_eq!(call.leave(12), Err(CoreError::WrongBridge));
        assert_eq!(call.leave(11).map(|value| value.bridge()), Ok(None));
        let ended = call.apply(CallEvent::Hangup).call();
        assert_eq!(ended.bridge(), None);
        assert_eq!(ended.join(11), Err(CoreError::Ended));
    }
}
