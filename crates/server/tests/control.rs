use rusterisk_api::{ChannelId, EventKind};
use rusterisk_core::{AudioGate, MuteDirection};
use rusterisk_server::{Action, Control, ControlError, Effect, IncomingCall, Milestone, Signal};

#[path = "control/music.rs"]
mod music;

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
fn duplicate_bridge_reports_its_id_and_preserves_the_original() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.apply(
        Action::CreateBridge {
            id: id("conversation")?,
            name: "Original".into(),
        },
        "created",
        0,
    )?;
    let original = control.bridge(&id("conversation")?)?.clone();
    let result = control.apply(
        Action::CreateBridge {
            id: id("conversation")?,
            name: "Replacement".into(),
        },
        "later",
        1,
    );
    let Err(error) = result else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(error.status(), 409);
    assert_eq!(
        error.to_string(),
        "Bridge with id 'conversation' already exists"
    );
    assert_eq!(control.bridge(&id("conversation")?)?, &original);
    Ok(())
}

#[test]
fn bridge_membership_keeps_its_generation_until_explicit_deletion() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("app")?;
    control.apply(originate("phone", 30)?, "now", 0)?;
    control.signal(
        Signal::Milestone {
            key: control.key(&id("phone")?)?,
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    let mut generations = Vec::new();
    for _ in 0..2 {
        control.apply(
            Action::CreateBridge {
                id: id("mix")?,
                name: String::new(),
            },
            "now",
            0,
        )?;
        let added = control.apply(
            Action::AddChannels {
                bridge: id("mix")?,
                channels: [id("phone")?].into_iter().collect(),
            },
            "now",
            0,
        )?;
        let [Effect::Bridge { generation, .. }] = &added.effects[..] else {
            return Err(ControlError::Unsupported);
        };
        let generation = *generation;
        generations.push(generation);
        let removed = control.apply(
            Action::RemoveChannels {
                bridge: id("mix")?,
                channels: [id("phone")?].into_iter().collect(),
            },
            "now",
            0,
        )?;
        assert!(
            matches!(&removed.effects[..], [Effect::Bridge { generation: current, channels, .. }] if *current == generation && channels.is_empty())
        );
        let deleted = control.apply(Action::DestroyBridge(id("mix")?), "now", 0)?;
        assert!(
            matches!(deleted.effects.last(), Some(Effect::ReleaseBridge { generation: current }) if *current == generation)
        );
    }
    assert_ne!(generations[0], generations[1]);
    Ok(())
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

fn incoming() -> Result<IncomingCall, ControlError> {
    Ok(IncomingCall {
        id: id("incoming")?,
        endpoint: "phone".into(),
        caller: "spoofed-display".into(),
        context: "internal".into(),
        extension: "100".into(),
        target: rusterisk_server::IncomingTarget::Stasis {
            application: "app".into(),
            arguments: ["inbound".into()].into_iter().collect(),
        },
    })
}

#[test]
fn incoming_stasis_entry_precedes_answer_and_terminal_cleanup_is_balanced(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    let admitted = control.incoming(incoming()?, "now")?;
    assert!(admitted.effects.is_empty());
    assert!(
        matches!(&admitted.events[1].kind, EventKind::StasisStart { channel, args } if channel.state == rusterisk_api::ChannelState::Ring && args == &["inbound"])
    );
    let channel = control.channel(&id("incoming")?)?;
    assert_eq!(channel.caller.number, "spoofed-display");
    assert_eq!(channel.dialplan.context, "internal");
    assert_eq!(channel.dialplan.exten, "100");
    let ended = control.apply(Action::Hangup(id("incoming")?), "later", 0)?;
    assert!(matches!(&ended.events[0].kind, EventKind::StasisEnd { .. }));
    assert!(matches!(
        &ended.events[1].kind,
        EventKind::ChannelDestroyed { .. }
    ));
    control.incoming(incoming()?, "again")?;
    let key = control.key(&id("incoming")?)?;
    let answered = control.signal(
        Signal::Milestone {
            key,
            milestone: Milestone::Answered,
        },
        "answer",
    )?;
    assert!(answered
        .events
        .iter()
        .all(|event| !matches!(event.kind, EventKind::StasisStart { .. })));
    assert_eq!(
        control.channel(&id("incoming")?)?.state,
        rusterisk_api::ChannelState::Up
    );
    Ok(())
}

#[test]
fn incoming_admission_requires_a_subscribed_app_and_preserves_existing_calls(
) -> Result<(), ControlError> {
    let mut control = Control::new(1);
    assert!(control.incoming(incoming()?, "now").is_err());
    control.subscribe("app")?;
    control.incoming(incoming()?, "now")?;
    assert!(control.incoming(incoming()?, "duplicate").is_err());
    assert_eq!(control.channel(&id("incoming")?)?.creationtime, "now");
    Ok(())
}

#[test]
fn answering_requests_a_signaling_effect_without_predicting_peer_or_media_milestones(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(incoming()?, "now")?;
    let ringing = control.apply(Action::Ring(id("incoming")?), "ring", 0)?;
    assert!(matches!(&ringing.effects[0], Effect::Ring { .. }));
    let accepted = control.apply(Action::Answer(id("incoming")?), "accept", 0)?;
    assert_eq!(accepted.status, 204);
    assert!(accepted.events.is_empty());
    assert!(matches!(&accepted.effects[0], Effect::Answer { .. }));
    assert_eq!(
        control.channel(&id("incoming")?)?.state,
        rusterisk_api::ChannelState::Ring
    );
    control.signal(
        Signal::Milestone {
            key: control.key(&id("incoming")?)?,
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert!(control
        .apply(Action::Answer(id("incoming")?), "duplicate", 0)?
        .effects
        .is_empty());
    control.apply(originate("outgoing", 30)?, "now", 0)?;
    assert!(matches!(
        control.apply(Action::Answer(id("outgoing")?), "wrong", 0),
        Err(ControlError::Conflict(_))
    ));
    Ok(())
}

#[test]
fn media_controls_preserve_muting_through_hold_and_apply_preanswer_settings_after_answer(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(incoming()?, "now")?;
    let channel = id("incoming")?;
    let muted = control.apply(
        Action::Mute {
            channel: channel.clone(),
            direction: MuteDirection::Incoming,
        },
        "mute",
        0,
    )?;
    assert!(muted.effects.is_empty());
    let answered = control.signal(
        Signal::Milestone {
            key: control.key(&channel)?,
            milestone: Milestone::Answered,
        },
        "answer",
    )?;
    assert!(matches!(
        &answered.effects[0],
        Effect::Gate {
            gate: AudioGate::IncomingMuted,
            ..
        }
    ));
    let held = control.apply(Action::Hold(channel.clone()), "hold", 0)?;
    assert!(matches!(
        &held.effects[0],
        Effect::Gate {
            gate: AudioGate::Closed,
            ..
        }
    ));
    assert!(matches!(
        &held.events[0].kind,
        EventKind::ChannelHold { .. }
    ));
    assert!(control
        .apply(Action::Hold(channel.clone()), "duplicate", 0)?
        .events
        .is_empty());
    let resumed = control.apply(Action::Unhold(channel.clone()), "resume", 0)?;
    assert!(matches!(
        &resumed.effects[0],
        Effect::Gate {
            gate: AudioGate::IncomingMuted,
            ..
        }
    ));
    assert!(matches!(
        &resumed.events[0].kind,
        EventKind::ChannelUnhold { .. }
    ));
    let unmuted = control.apply(
        Action::Unmute {
            channel,
            direction: MuteDirection::Both,
        },
        "unmute",
        0,
    )?;
    assert!(matches!(
        &unmuted.effects[0],
        Effect::Gate {
            gate: AudioGate::Open,
            ..
        }
    ));
    Ok(())
}

#[test]
fn received_digits_require_the_current_answered_channel_generation() -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(incoming()?, "now")?;
    let key = control.key(&id("incoming")?)?;
    let signal = Signal::DtmfReceived {
        key: key.clone(),
        digit: rusterisk_core::DtmfDigit::Hash,
        duration_ms: 100,
    };
    assert!(control.signal(signal.clone(), "early")?.events.is_empty());
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answer",
    )?;
    let received = control.signal(signal.clone(), "digit")?;
    assert!(
        matches!(&received.events[0].kind, EventKind::ChannelDtmfReceived { digit, duration_ms: 100, .. } if digit == "#")
    );
    control.apply(Action::Hangup(key.id.clone()), "end", 0)?;
    control.incoming(incoming()?, "replacement")?;
    assert!(control.signal(signal, "late")?.events.is_empty());
    Ok(())
}
