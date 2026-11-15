use rusterisk_api::{
    Channel, ChannelId, ChannelState, MediaControlFormat, MediaDirection, MediaFormat,
};
use rusterisk_config::Config;
use rusterisk_server::{Action, Control, ControlError, Effect, Milestone, Signal};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn create(client: &str) -> Result<Action, ControlError> {
    Ok(Action::ExternalWebSocketClient {
        id: id("external")?,
        app: "calls".into(),
        client: client.into(),
        format: MediaFormat::Pcmu,
        control_format: None,
        direction: MediaDirection::Both,
        auto_answer: true,
        data: "media,agent".into(),
    })
}

#[test]
fn client_admission_captures_configuration_and_keeps_call_milestones_independent(
) -> Result<(), ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test-password\n", "", "", "").map_err(|error| ControlError::Adapter(error.to_string()))?
        .with_websocket_configuration("[global]\ncontrol_message_format=json\n", "[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://127.0.0.1:18080/media\nprotocols=media\nreconnect_attempts=1\n").map_err(|error| ControlError::Adapter(error.to_string()))?;
    let mut control = Control::with_configuration(4, &config);
    control.subscribe("calls")?;
    assert!(matches!(
        control.apply(create("missing")?, "missing", 0),
        Err(ControlError::Invalid(
            "external_host must be a valid websocket_client connection id."
        ))
    ));
    let accepted = control.apply_with_variables(
        create("agent")?,
        std::collections::BTreeMap::from([("INITIAL_TAG".into(), "outgoing value".into())]),
        "created",
        0,
    )?;
    let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
    assert!(channel.name.starts_with("WebSocket/agent/"));
    assert_eq!(channel.state, ChannelState::Down);
    assert_eq!(
        channel
            .channelvars
            .ok_or(ControlError::Capacity)?
            .get("MEDIA_WEBSOCKET_CONNECTION_ID")
            .map(String::as_str),
        Some("agent")
    );
    let key = control.key(&channel.id)?;
    assert!(
        matches!(&accepted.effects[..], [Effect::ConnectWebSocket { key: current, client, options, channel_variables }] if current == &key && client.uri.expose() == "ws://127.0.0.1:18080/media" && options.control_format == MediaControlFormat::Json && channel_variables.get("INITIAL_TAG").map(String::as_str) == Some("outgoing value"))
    );
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "early",
    )?;
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Down);
    let admitted = control.signal(
        Signal::WebSocketAdmitted {
            key: key.clone(),
            result: Ok(()),
        },
        "admitted",
    )?;
    assert!(admitted.body.is_some());
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Down);
    let answered = control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert!(answered.events.iter().any(|event| matches!(&event.kind, rusterisk_api::EventKind::StasisStart { args, .. } if args == &["media", "agent"])));
    control.apply(Action::Hangup(channel.id.clone()), "ended", 1)?;
    control.apply(create("agent")?, "replacement", 2)?;
    let replacement = control.key(&channel.id)?;
    let late = control.signal(
        Signal::WebSocketAdmitted {
            key: key.clone(),
            result: Ok(()),
        },
        "late",
    )?;
    assert!(matches!(&late.effects[..], [Effect::Release { key: released }] if released == &key));
    assert_eq!(control.key(&channel.id)?, replacement);
    control.signal(
        Signal::WebSocketAdmitted {
            key: replacement,
            result: Err(ControlError::Capacity),
        },
        "failed",
    )?;
    assert!(matches!(
        control.channel(&channel.id),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
