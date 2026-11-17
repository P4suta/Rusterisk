use super::{id, incoming};
use rusterisk_core::{AudioGate, MuteDirection, PeerMediaDirection};
use rusterisk_server::{
    Action, CallKey, Control, ControlError, Effect, Milestone, Outcome, Signal,
};

fn projection(outcome: Outcome) -> Result<(CallKey, AudioGate, AudioGate), ControlError> {
    match outcome.effects.as_slice() {
        [Effect::Gate {
            key,
            gate,
            generated_audio_gate,
        }]
        | [Effect::PeerDirection {
            key,
            gate,
            generated_audio_gate,
            ..
        }] => Ok((key.clone(), *gate, *generated_audio_gate)),
        _ => Err(ControlError::Invalid(
            "Expected one complete audio gate projection",
        )),
    }
}

#[test]
fn hold_mute_and_direction_project_conversation_and_music_permissions_together(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(incoming()?, "now")?;
    let channel = id("incoming")?;
    let key = control.key(&channel)?;
    control.apply(
        Action::Mute {
            channel: channel.clone(),
            direction: MuteDirection::Incoming,
        },
        "before answer",
        0,
    )?;
    assert_eq!(
        projection(control.signal(
            Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
        )?)?,
        (
            key.clone(),
            AudioGate::IncomingMuted,
            AudioGate::IncomingMuted
        )
    );
    assert_eq!(
        projection(control.apply(
            Action::Unmute {
                channel: channel.clone(),
                direction: MuteDirection::Both,
            },
            "unmuted",
            0,
        )?)?,
        (key.clone(), AudioGate::Open, AudioGate::Open)
    );
    for (action, voice, music) in [
        (
            Action::Hold(channel.clone()),
            AudioGate::Closed,
            AudioGate::Open,
        ),
        (
            Action::Mute {
                channel: channel.clone(),
                direction: MuteDirection::Incoming,
            },
            AudioGate::Closed,
            AudioGate::IncomingMuted,
        ),
        (
            Action::Mute {
                channel: channel.clone(),
                direction: MuteDirection::Outgoing,
            },
            AudioGate::Closed,
            AudioGate::Closed,
        ),
        (
            Action::Unmute {
                channel: channel.clone(),
                direction: MuteDirection::Incoming,
            },
            AudioGate::Closed,
            AudioGate::OutgoingMuted,
        ),
    ] {
        assert_eq!(
            projection(control.apply(action, "changed", 0)?)?,
            (key.clone(), voice, music)
        );
    }
    assert_eq!(
        projection(control.signal(
            Signal::PeerDirection {
                key: key.clone(),
                direction: PeerMediaDirection::ReceiveOnly,
            },
            "direction",
        )?)?,
        (key.clone(), AudioGate::Closed, AudioGate::Closed)
    );
    assert_eq!(
        projection(control.apply(
            Action::Unmute {
                channel: channel.clone(),
                direction: MuteDirection::Both,
            },
            "unmuted",
            0,
        )?)?,
        (key.clone(), AudioGate::Closed, AudioGate::IncomingMuted)
    );
    assert_eq!(
        projection(control.apply(Action::Unhold(channel), "resumed", 0)?)?,
        (key, AudioGate::IncomingMuted, AudioGate::IncomingMuted)
    );
    Ok(())
}

#[test]
fn stale_direction_updates_cannot_project_either_gate_into_a_replacement_generation(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(incoming()?, "first")?;
    let channel = id("incoming")?;
    let first = control.key(&channel)?;
    control.apply(Action::Hangup(channel.clone()), "ended", 0)?;
    control.incoming(incoming()?, "replacement")?;
    let replacement = control.key(&channel)?;
    assert_ne!(first.generation, replacement.generation);
    control.signal(
        Signal::Milestone {
            key: replacement.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    let stale = control.signal(
        Signal::PeerDirection {
            key: first,
            direction: PeerMediaDirection::Inactive,
        },
        "stale",
    )?;
    assert!(stale.effects.is_empty());
    assert!(stale.events.is_empty());
    assert_eq!(
        projection(control.signal(
            Signal::PeerDirection {
                key: replacement.clone(),
                direction: PeerMediaDirection::ReceiveOnly,
            },
            "current",
        )?)?,
        (
            replacement,
            AudioGate::IncomingMuted,
            AudioGate::IncomingMuted
        )
    );
    Ok(())
}
