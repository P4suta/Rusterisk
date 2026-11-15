use rusterisk_api::{
    Channel, ChannelId, ChannelState, MediaControlFormat, MediaDirection, MediaFormat,
};
use rusterisk_server::{Action, Control, ControlError, Milestone, Signal};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn create(connection: &str) -> Result<Action, ControlError> {
    Ok(Action::ExternalWebSocket {
        id: id("external")?,
        app: "calls".into(),
        connection_id: id(connection)?,
        format: MediaFormat::Pcmu,
        control_format: MediaControlFormat::Json,
        direction: MediaDirection::Both,
        auto_answer: true,
        data: "session".into(),
    })
}

#[test]
fn incoming_media_connections_publish_variables_and_claim_one_channel_generation(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let accepted = control.apply(create("connection")?, "created", 0)?;
    let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(channel.state, ChannelState::Down);
    let variables = channel.channelvars.ok_or(ControlError::Capacity)?;
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_CONNECTION_ID")
            .map(String::as_str),
        Some("connection")
    );
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE")
            .map(String::as_str),
        Some("160")
    );
    let original = control.key(&channel.id)?;
    control.signal(
        Signal::Milestone {
            key: original.clone(),
            milestone: Milestone::Answered,
        },
        "early",
    )?;
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Down);
    let (claimed, options) = control.claim_media_connection(&id("connection")?)?;
    assert_eq!(claimed, original);
    assert_eq!(options.format, MediaFormat::Pcmu);
    assert!(matches!(
        control.claim_media_connection(&id("connection")?),
        Err(ControlError::Conflict(
            "Another websocket connection exists for this connection id"
        ))
    ));
    control.signal(
        Signal::Milestone {
            key: original.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Up);
    control.apply(Action::Hangup(channel.id.clone()), "ended", 1)?;
    assert!(matches!(
        control.claim_media_connection(&id("connection")?),
        Err(ControlError::NotFound("WebSocket instance not found"))
    ));
    control.apply(create("replacement")?, "replacement", 2)?;
    let replacement = control.key(&channel.id)?;
    control.signal(
        Signal::Terminated {
            key: original,
            cause: 16,
        },
        "late",
    )?;
    assert_eq!(control.key(&channel.id)?, replacement);
    assert_eq!(
        control.claim_media_connection(&id("replacement")?)?.0,
        replacement
    );
    Ok(())
}

#[test]
fn unconnected_media_channels_expire_at_the_reference_originate_deadline(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.apply(create("waiting")?, "created", 100)?;
    control.expire(30099, "before")?;
    assert!(control.channel(&id("external")?).is_ok());
    control.expire(30100, "expired")?;
    assert!(matches!(
        control.claim_media_connection(&id("waiting")?),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
