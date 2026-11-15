use rusterisk_api::{
    Channel, ChannelId, ChannelState, EventKind, ExternalRtpTarget, StaticRtpCodec,
};
use rusterisk_server::{Action, Control, ControlError, Effect, Milestone, Signal};

fn id() -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from("external".to_owned())?)
}

fn create() -> Result<Action, ControlError> {
    Ok(Action::ExternalRtp {
        id: id()?,
        app: "calls".into(),
        target: ExternalRtpTarget::new(
            "127.0.0.1:12000"
                .parse()
                .map_err(|_| ControlError::Unsupported)?,
            StaticRtpCodec::Pcmu,
        )
        .map_err(|_| ControlError::Unsupported)?,
        data: "media".into(),
    })
}

#[test]
fn external_admission_reports_local_variables_without_predicting_stasis_or_received_audio(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let accepted = control.apply(create()?, "created", 0)?;
    let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(channel.state, ChannelState::Down);
    assert!(channel.channelvars.is_none());
    assert!(
        matches!(&accepted.effects[..], [Effect::ConnectExternalRtp { key, .. }] if key.id == id()?)
    );
    let key = control.key(&id()?)?;
    let admitted = control.signal(
        Signal::ExternalRtpAdmitted {
            key: key.clone(),
            result: Ok("127.0.0.1:10000"
                .parse()
                .map_err(|_| ControlError::Unsupported)?),
        },
        "admitted",
    )?;
    let channel: Channel = serde_json::from_value(admitted.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(channel.state, ChannelState::Down);
    let variables = channel.channelvars.ok_or(ControlError::Capacity)?;
    assert_eq!(
        variables
            .get("UNICASTRTP_LOCAL_ADDRESS")
            .map(String::as_str),
        Some("127.0.0.1")
    );
    assert_eq!(
        variables.get("UNICASTRTP_LOCAL_PORT").map(String::as_str),
        Some("10000")
    );
    assert!(admitted.events.is_empty());
    let answered = control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert_eq!(
        answered
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::StasisStart { .. }))
            .count(),
        1
    );
    assert_eq!(control.channel(&id()?)?.state, ChannelState::Up);
    let media = control.apply(
        Action::GetVariable {
            channel: id()?,
            variable: "RUSTERISK_MEDIA_CONNECTED".into(),
        },
        "now",
        0,
    )?;
    assert_eq!(media.body, Some(serde_json::json!({"value":"0"})));
    control.signal(
        Signal::Milestone {
            key,
            milestone: Milestone::MediaConnected,
        },
        "audio",
    )?;
    let media = control.apply(
        Action::GetVariable {
            channel: id()?,
            variable: "RUSTERISK_MEDIA_CONNECTED".into(),
        },
        "now",
        0,
    )?;
    assert_eq!(media.body, Some(serde_json::json!({"value":"1"})));
    Ok(())
}

#[test]
fn failed_and_late_external_admission_release_only_their_channel_generation(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.apply(create()?, "first", 0)?;
    let first = control.key(&id()?)?;
    let failed = control.signal(
        Signal::ExternalRtpAdmitted {
            key: first.clone(),
            result: Err(ControlError::Capacity),
        },
        "failed",
    )?;
    assert!(matches!(&failed.effects[..], [Effect::Release { key }] if key == &first));
    assert!(matches!(
        control.channel(&id()?),
        Err(ControlError::NotFound(_))
    ));
    control.apply(create()?, "replacement", 1)?;
    let replacement = control.key(&id()?)?;
    let late = control.signal(
        Signal::ExternalRtpAdmitted {
            key: first.clone(),
            result: Ok("127.0.0.1:10000"
                .parse()
                .map_err(|_| ControlError::Unsupported)?),
        },
        "late",
    )?;
    assert!(matches!(&late.effects[..], [Effect::Release { key }] if key == &first));
    assert_eq!(control.key(&id()?)?, replacement);
    assert_eq!(control.channel(&id()?)?.state, ChannelState::Down);
    assert!(control.channel(&id()?)?.channelvars.is_none());
    Ok(())
}
