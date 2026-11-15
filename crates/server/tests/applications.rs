use rusterisk_api::{Application, ChannelId};
use rusterisk_server::{Action, Control, ControlError, Milestone, Signal};

fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}

fn application(control: &mut Control, name: &str) -> Result<Application, ControlError> {
    let outcome = control.apply(Action::GetApplication(name.into()), "now", 0)?;
    assert!(outcome.effects.is_empty());
    assert!(outcome.events.is_empty());
    Ok(serde_json::from_value(
        outcome.body.ok_or(ControlError::Capacity)?,
    )?)
}

#[test]
fn wildcard_subscription_snapshots_use_the_reference_topic_identifiers() -> Result<(), ControlError>
{
    let mut control = Control::new(4);
    control.subscribe("events")?;
    control.apply(
        Action::SubscribeResources {
            application: "events".into(),
            sources: ["channel:".to_owned(), "bridge:".to_owned()]
                .into_iter()
                .collect(),
        },
        "now",
        0,
    )?;
    let snapshot = application(&mut control, "events")?;
    assert_eq!(snapshot.channel_ids, [id("__AST_CHANNEL_ALL_TOPIC")?]);
    assert_eq!(snapshot.bridge_ids, [id("__AST_BRIDGE_ALL_TOPIC")?]);
    Ok(())
}

#[test]
fn app_queries_follow_reserved_channels_moves_and_bridge_membership() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let empty = control.apply(Action::ListApplications, "now", 0)?;
    assert_eq!(empty.body, Some(serde_json::json!([])));
    for app in ["first", "second"] {
        control.subscribe(app)?;
    }
    for channel in ["alice", "bob"] {
        control.apply(
            Action::Originate {
                id: id(channel)?,
                endpoint: "PJSIP/phone".into(),
                app: "first".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 30,
            },
            "now",
            0,
        )?;
    }
    let reserved = application(&mut control, "first")?;
    assert_eq!(reserved.name, "first");
    assert_eq!(reserved.channel_ids, [id("alice")?, id("bob")?]);
    assert!(reserved.bridge_ids.is_empty());
    assert!(reserved.endpoint_ids.is_empty());
    assert!(reserved.device_names.is_empty());
    for channel in ["alice", "bob"] {
        control.signal(
            Signal::Milestone {
                key: control.key(&id(channel)?)?,
                milestone: Milestone::Answered,
            },
            "now",
        )?;
    }
    for bridge in ["mix", "empty"] {
        control.apply(
            Action::CreateBridge {
                id: id(bridge)?,
                name: String::new(),
            },
            "now",
            0,
        )?;
    }
    control.apply(
        Action::AddChannels {
            bridge: id("mix")?,
            channels: [id("alice")?, id("bob")?].into_iter().collect(),
        },
        "now",
        0,
    )?;
    assert_eq!(application(&mut control, "first")?.bridge_ids, [id("mix")?]);
    control.apply(
        Action::Move {
            channel: id("alice")?,
            app: "second".into(),
            args: Vec::new(),
        },
        "now",
        0,
    )?;
    assert_eq!(
        application(&mut control, "first")?.channel_ids,
        [id("bob")?]
    );
    assert_eq!(
        application(&mut control, "second")?.channel_ids,
        [id("alice")?]
    );
    assert_eq!(
        application(&mut control, "second")?.bridge_ids,
        [id("mix")?]
    );
    control.apply(
        Action::RemoveChannels {
            bridge: id("mix")?,
            channels: [id("bob")?].into_iter().collect(),
        },
        "now",
        0,
    )?;
    assert!(application(&mut control, "first")?.bridge_ids.is_empty());
    control.apply(Action::Hangup(id("alice")?), "now", 0)?;
    assert!(application(&mut control, "second")?.channel_ids.is_empty());
    assert!(application(&mut control, "second")?.bridge_ids.is_empty());
    control.unsubscribe("first", "now")?;
    assert!(matches!(
        application(&mut control, "first"),
        Err(ControlError::NotFound(_))
    ));
    let listed = control.apply(Action::ListApplications, "now", 0)?;
    let listed: Vec<Application> =
        serde_json::from_value(listed.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "second");
    Ok(())
}
