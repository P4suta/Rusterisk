use rusterisk_api::{ChannelId, EventKind};
use rusterisk_config::Dialplan;
use rusterisk_server::{
    Action, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn program(source: &str) -> Result<Dialplan, ControlError> {
    Dialplan::parse(source).map_err(|error| ControlError::Adapter(error.to_string()))
}

#[test]
fn continuing_leaves_the_bridge_and_resumes_after_stasis_without_restarting_tokens(
) -> Result<(), ControlError> {
    let program = program("[internal]\nexten => 200,1,Stasis(calls,first)\nsame => n,Set(RESUMED=yes)\nsame => n,Stasis(calls,second,${RESUMED})\n")?;
    let mut control = Control::with_dialplan(4, program.clone());
    control.subscribe("calls")?;
    let channel = id("incoming")?;
    control.incoming(
        IncomingCall {
            id: channel.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: IncomingTarget::Dialplan,
        },
        "now",
    )?;
    let key = control.key(&channel)?;
    control.start_dialplan(&key, program, 0, "now")?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    control.apply(
        Action::CreateBridge {
            id: id("mix")?,
            name: "Conversation".into(),
        },
        "now",
        0,
    )?;
    control.apply(
        Action::AddChannels {
            bridge: id("mix")?,
            channels: [channel.clone()].into_iter().collect(),
        },
        "now",
        0,
    )?;
    let continued = control.apply(
        Action::Continue {
            channel: channel.clone(),
            context: None,
            extension: None,
            priority: None,
        },
        "now",
        0,
    )?;
    assert!(
        matches!(&continued.effects[..], [Effect::Bridge { channels, .. }] if channels.is_empty())
    );
    assert!(matches!(
        &continued.events[0].kind,
        EventKind::ChannelLeftBridge { .. }
    ));
    assert!(
        matches!(&continued.events[1].kind, EventKind::StasisEnd { channel: ended } if ended.id == channel)
    );
    assert!(control.bridge(&id("mix")?)?.channels.is_empty());
    assert!(matches!(
        control.apply(Action::Answer(channel.clone()), "now", 0),
        Err(ControlError::Conflict(_))
    ));
    let resumed = control.tick_dialplans(100, "later")?;
    assert!(
        matches!(&resumed.events[1].kind, EventKind::StasisStart { channel: resumed, args } if resumed.dialplan.priority == 3 && args == &["second", "yes"])
    );
    assert_eq!(control.key(&channel)?, key);
    Ok(())
}

#[test]
fn an_originated_channel_keeps_a_program_for_explicit_context_continuation(
) -> Result<(), ControlError> {
    let mut control = Control::with_dialplan(
        4,
        program("[destination]\nexten => s,1,Set(X=arrived)\nsame => n,Stasis(calls,${X})\n")?,
    );
    control.subscribe("calls")?;
    let channel = id("outgoing")?;
    control.apply(
        Action::Originate {
            id: channel.clone(),
            endpoint: "PJSIP/phone".into(),
            app: "calls".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "now",
        0,
    )?;
    control.signal(
        Signal::Milestone {
            key: control.key(&channel)?,
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    control.apply(
        Action::Continue {
            channel: channel.clone(),
            context: Some("destination".into()),
            extension: None,
            priority: None,
        },
        "now",
        0,
    )?;
    let resumed = control.tick_dialplans(100, "later")?;
    assert!(
        matches!(&resumed.events[1].kind, EventKind::StasisStart { channel: resumed, args } if resumed.dialplan.context == "destination" && resumed.dialplan.exten == "s" && args == &["arrived"])
    );
    let ended = control.apply(
        Action::Continue {
            channel: channel.clone(),
            context: None,
            extension: Some("missing".into()),
            priority: Some(9),
        },
        "later",
        100,
    )?;
    assert!(matches!(&ended.events[0].kind, EventKind::StasisEnd { .. }));
    let ended = control.tick_dialplans(200, "later")?;
    assert!(matches!(&ended.effects[..], [Effect::Release { .. }]));
    assert!(control.channel(&channel).is_err());
    Ok(())
}
