use rusterisk_api::{AudioSocketTarget, Channel, ChannelId, ChannelState, EventKind};
use rusterisk_server::{Action, Control, ControlError, Effect, Milestone, Signal};

fn create() -> Result<Action, ControlError> {
    Ok(Action::ExternalAudioSocket {
        id: ChannelId::try_from("external".to_owned())?,
        app: "calls".into(),
        target: AudioSocketTarget::new(
            "127.0.0.1:12000"
                .parse()
                .map_err(|_| ControlError::Unsupported)?,
            [7; 16],
        )
        .map_err(|_| ControlError::Unsupported)?,
    })
}

#[test]
fn stream_admission_preserves_session_arguments_and_independent_call_milestones(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let accepted = control.apply(create()?, "created", 0)?;
    let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
    assert!(channel.name.starts_with("AudioSocket/"));
    assert_eq!(channel.state, ChannelState::Down);
    let key = control.key(&channel.id)?;
    assert!(
        matches!(&accepted.effects[..], [Effect::ConnectAudioSocket { key: current, .. }] if current == &key)
    );
    let early = control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "early",
    )?;
    assert!(early.events.is_empty());
    let admitted = control.signal(
        Signal::AudioSocketAdmitted {
            key: key.clone(),
            result: Ok(()),
        },
        "admitted",
    )?;
    let channel: Channel = serde_json::from_value(admitted.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(channel.state, ChannelState::Down);
    assert!(channel.channelvars.is_none());
    let answered = control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert!(answered.events.iter().any(|event| matches!(&event.kind, EventKind::StasisStart { args, .. } if args == &["07070707-0707-0707-0707-070707070707"])));
    control.apply(Action::Hangup(channel.id.clone()), "ended", 1)?;
    control.apply(create()?, "replacement", 2)?;
    let replacement = control.key(&channel.id)?;
    let late = control.signal(
        Signal::AudioSocketAdmitted {
            key: key.clone(),
            result: Ok(()),
        },
        "late",
    )?;
    assert!(matches!(&late.effects[..], [Effect::Release { key: current }] if current == &key));
    assert_eq!(control.key(&channel.id)?, replacement);
    let failed = control.signal(
        Signal::AudioSocketAdmitted {
            key: replacement,
            result: Err(ControlError::Capacity),
        },
        "failed",
    )?;
    assert!(matches!(&failed.effects[..], [Effect::Release { .. }]));
    assert!(matches!(
        control.channel(&channel.id),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
