use rusterisk_api::{ChannelId, EventKind};
use rusterisk_core::{AudioGate, MuteDirection, PeerMediaDirection};
use rusterisk_server::{Action, CallKey, Control, ControlError, Effect, Milestone, Signal};

fn originate(control: &mut Control) -> Result<CallKey, ControlError> {
    let id = ChannelId::try_from("call".to_owned())?;
    control.apply(
        Action::Originate {
            id: id.clone(),
            endpoint: "PJSIP/phone".into(),
            app: "app".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "now",
        0,
    )?;
    control.key(&id)
}

fn direction(
    control: &mut Control,
    key: &CallKey,
    direction: PeerMediaDirection,
    gate: AudioGate,
) -> Result<(), ControlError> {
    let outcome = control.signal(
        Signal::PeerDirection {
            key: key.clone(),
            direction,
        },
        "now",
    )?;
    assert!(
        matches!(&outcome.effects[..], [Effect::PeerDirection { key: actual, direction: actual_direction, gate: actual_gate, .. }] if actual == key && *actual_direction == direction && *actual_gate == gate)
    );
    Ok(())
}

#[test]
fn peer_resume_preserves_local_audio_controls() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    let key = originate(&mut control)?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    control.apply(
        Action::Mute {
            channel: key.id.clone(),
            direction: MuteDirection::Incoming,
        },
        "now",
        0,
    )?;
    direction(
        &mut control,
        &key,
        PeerMediaDirection::SendOnly,
        AudioGate::Closed,
    )?;
    direction(
        &mut control,
        &key,
        PeerMediaDirection::SendReceive,
        AudioGate::IncomingMuted,
    )?;
    control.apply(Action::Hold(key.id.clone()), "now", 0)?;
    direction(
        &mut control,
        &key,
        PeerMediaDirection::Inactive,
        AudioGate::Closed,
    )?;
    direction(
        &mut control,
        &key,
        PeerMediaDirection::SendReceive,
        AudioGate::Closed,
    )?;
    let resumed = control.apply(Action::Unhold(key.id), "now", 0)?;
    assert!(matches!(
        &resumed.effects[..],
        [Effect::Gate {
            gate: AudioGate::IncomingMuted,
            ..
        }]
    ));
    Ok(())
}

#[test]
fn peer_hold_notifications_follow_remote_direction_independently_of_local_hold(
) -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    let key = originate(&mut control)?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    control.apply(Action::Hold(key.id.clone()), "now", 0)?;
    for (direction, notification) in [
        (PeerMediaDirection::ReceiveOnly, None),
        (PeerMediaDirection::SendOnly, Some(true)),
        (PeerMediaDirection::Inactive, None),
        (PeerMediaDirection::SendReceive, Some(false)),
        (PeerMediaDirection::SendReceive, None),
    ] {
        let outcome = control.signal(
            Signal::PeerDirection {
                key: key.clone(),
                direction,
            },
            "now",
        )?;
        match notification {
            None => assert!(outcome.events.is_empty()),
            Some(true) => assert!(
                matches!(&outcome.events[..], [event] if matches!(&event.kind, EventKind::ChannelHold { channel, musicclass: Some(class) } if channel.id == key.id && class == "default"))
            ),
            Some(false) => assert!(
                matches!(&outcome.events[..], [event] if matches!(&event.kind, EventKind::ChannelUnhold { channel } if channel.id == key.id))
            ),
        }
    }
    let resumed = control.apply(Action::Unhold(key.id), "now", 0)?;
    assert!(matches!(
        &resumed.effects[..],
        [Effect::Gate {
            gate: AudioGate::Open,
            ..
        }]
    ));
    Ok(())
}

#[test]
fn initial_direction_waits_for_answer_and_duplicate_updates_have_no_effect(
) -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    let key = originate(&mut control)?;
    let signal = Signal::PeerDirection {
        key: key.clone(),
        direction: PeerMediaDirection::SendOnly,
    };
    assert!(control.signal(signal.clone(), "now")?.effects.is_empty());
    let answered = control.signal(
        Signal::Milestone {
            key,
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    assert!(answered.effects.iter().any(|effect| matches!(
        effect,
        Effect::Gate {
            gate: AudioGate::OutgoingMuted,
            ..
        }
    )));
    assert!(control.signal(signal, "now")?.effects.is_empty());
    Ok(())
}

#[test]
fn stale_and_ended_direction_updates_do_not_control_a_replacement() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    let key = originate(&mut control)?;
    control.apply(Action::Hangup(key.id.clone()), "now", 0)?;
    let signal = Signal::PeerDirection {
        key: key.clone(),
        direction: PeerMediaDirection::Inactive,
    };
    assert!(control.signal(signal.clone(), "now")?.effects.is_empty());
    let current = originate(&mut control)?;
    assert_ne!(current, key);
    assert!(control.signal(signal, "now")?.effects.is_empty());
    control.signal(
        Signal::Milestone {
            key: current.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    direction(
        &mut control,
        &current,
        PeerMediaDirection::ReceiveOnly,
        AudioGate::IncomingMuted,
    )?;
    Ok(())
}
