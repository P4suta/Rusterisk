use rusterisk_api::{ChannelId, EventKind};
use rusterisk_server::{Action, Control, ControlError, Effect, Milestone, Signal};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn originate(name: &str, timeout: u32) -> Result<Action, ControlError> {
    Ok(Action::Originate {
        id: id(name)?,
        endpoint: "PJSIP/phone".into(),
        app: "app".into(),
        args: Vec::new(),
        caller: "100".into(),
        timeout,
    })
}

#[test]
fn originate_acceptance_is_separate_from_answer_and_stasis() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("app")?;
    let outcome = control.apply(originate("outgoing", 30)?, "now", 0)?;
    assert_eq!(outcome.effects.len(), 1);
    assert!(matches!(&outcome.effects[0], Effect::Start { .. }));
    assert!(!outcome
        .events
        .iter()
        .any(|event| matches!(event.kind, EventKind::StasisStart { .. })));
    let key = control.key(&id("outgoing")?)?;
    let answered = control.signal(
        Signal::Milestone {
            key,
            milestone: Milestone::Answered,
        },
        "later",
    )?;
    assert!(answered
        .events
        .iter()
        .any(|event| matches!(event.kind, EventKind::StasisStart { .. })));
    Ok(())
}

#[test]
fn late_adapter_events_cannot_resurrect_or_control_a_reused_identifier() -> Result<(), ControlError>
{
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.apply(originate("call", 30)?, "now", 0)?;
    let first = control.key(&id("call")?)?;
    let ended = control.apply(Action::Hangup(id("call")?), "later", 1)?;
    assert!(ended
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { .. })));
    control.apply(originate("call", 30)?, "later", 2)?;
    let second = control.key(&id("call")?)?;
    assert_ne!(first, second);
    control.signal(
        Signal::Milestone {
            key: first,
            milestone: Milestone::Answered,
        },
        "late",
    )?;
    assert_eq!(
        control.channel(&id("call")?)?.state,
        rusterisk_api::ChannelState::Down
    );
    Ok(())
}

#[test]
fn bridge_updates_are_atomic_and_hangup_releases_membership() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("app")?;
    for name in ["alice", "bob"] {
        control.apply(originate(name, 30)?, "now", 0)?;
        control.signal(
            Signal::Milestone {
                key: control.key(&id(name)?)?,
                milestone: Milestone::Answered,
            },
            "now",
        )?;
    }
    control.apply(
        Action::CreateBridge {
            id: id("mix")?,
            name: "Conversation".into(),
        },
        "now",
        0,
    )?;
    assert!(control
        .apply(
            Action::AddChannels {
                bridge: id("mix")?,
                channels: [id("alice")?, id("missing")?].into_iter().collect()
            },
            "now",
            0
        )
        .is_err());
    assert!(control.bridge(&id("mix")?)?.channels.is_empty());
    control.apply(
        Action::AddChannels {
            bridge: id("mix")?,
            channels: [id("alice")?, id("bob")?].into_iter().collect(),
        },
        "now",
        0,
    )?;
    control.apply(Action::Hangup(id("alice")?), "later", 1)?;
    assert_eq!(control.bridge(&id("mix")?)?.channels, [id("bob")?]);
    Ok(())
}

#[test]
fn bounded_capacity_and_dial_timeouts_have_explicit_failures() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    control.apply(originate("first", 1)?, "now", 0)?;
    assert_eq!(
        control.apply(originate("second", 1)?, "now", 0).map(|_| ()),
        Err(ControlError::Capacity)
    );
    let expired = control.expire(1000, "later")?;
    assert!(expired
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { .. })));
    assert!(control.channel(&id("first")?).is_err());
    Ok(())
}

#[test]
fn shutdown_releases_every_call_and_is_idempotent() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("app")?;
    for name in ["first", "second"] {
        control.apply(originate(name, 30)?, "now", 0)?;
    }
    let outcome = control.shutdown("later")?;
    assert_eq!(outcome.effects.len(), 2);
    for name in ["first", "second"] {
        assert!(control.channel(&id(name)?).is_err());
    }
    assert!(control.shutdown("again")?.effects.is_empty());
    Ok(())
}
