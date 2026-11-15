use rusterisk_api::{ChannelId, EventKind};
use rusterisk_config::Dialplan;
use rusterisk_server::{
    Action, CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget,
};

fn start(
    capacity: usize,
    timeout: &str,
) -> Result<(Control, CallKey, rusterisk_server::Outcome), ControlError> {
    let mut control = Control::new(capacity);
    control.subscribe("calls")?;
    let id = ChannelId::try_from("incoming".to_owned())?;
    control.incoming(
        IncomingCall {
            id: id.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: IncomingTarget::Dialplan,
        },
        "now",
    )?;
    let key = control.key(&id)?;
    let program = Dialplan::parse(&format!("[internal]\nexten => 200,1,Dial(PJSIP/alice&PJSIP/bob{timeout})\nsame => n,Stasis(calls,${{DIALSTATUS}})\n")).map_err(|error| ControlError::Adapter(error.to_string()))?;
    let outcome = control.start_dialplan(&key, program, 50, "now")?;
    Ok((control, key, outcome))
}

#[test]
fn parallel_attempts_belong_to_the_parent_and_cancel_with_it() -> Result<(), ControlError> {
    for (timeout, expected) in [("", None), (",1", Some(1))] {
        let (mut control, parent, started) = start(4, timeout)?;
        assert_eq!(started.effects.len(), 2);
        let mut children = Vec::new();
        for (effect, destination) in started.effects.iter().zip(["PJSIP/alice", "PJSIP/bob"]) {
            let Effect::Start {
                key,
                endpoint,
                caller,
                timeout,
            } = effect
            else {
                return Err(ControlError::Unsupported);
            };
            assert_eq!(endpoint, destination);
            assert_eq!(caller, "100");
            assert_eq!(*timeout, expected);
            assert_ne!(key, &parent);
            children.push(key.clone());
        }
        assert_ne!(children[0], children[1]);
        assert!(started.events.is_empty());
        assert!(control.tick_dialplans(1049, "later")?.effects.is_empty());
        let ended = control.apply(Action::Hangup(parent.id.clone()), "later", 1049)?;
        assert_eq!(ended.effects.len(), 3);
        for key in children.iter().chain(std::iter::once(&parent)) {
            assert!(ended.effects.iter().any(
                |effect| matches!(effect, Effect::Release { key: released } if released == key)
            ));
            assert!(control.channel(&key.id).is_err());
        }
        assert!(control.tick_dialplans(2000, "later")?.effects.is_empty());
    }
    Ok(())
}

#[test]
fn dial_timeouts_release_all_attempts_and_capacity_failure_advances_without_children(
) -> Result<(), ControlError> {
    let (mut control, parent, started) = start(4, ",1")?;
    let expired = control.tick_dialplans(1050, "later")?;
    assert_eq!(expired.effects.len(), 2);
    for started in started.effects {
        let Effect::Start { key, .. } = started else {
            return Err(ControlError::Unsupported);
        };
        assert!(expired
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key: released } if released == &key)));
    }
    assert!(
        matches!(&expired.events[1].kind, EventKind::StasisStart { args, .. } if args == &["NOANSWER"])
    );
    assert!(control.channel(&parent.id).is_ok());
    let (control, parent, unavailable) = start(2, "")?;
    assert!(unavailable.effects.is_empty());
    assert!(
        matches!(&unavailable.events[1].kind, EventKind::StasisStart { args, .. } if args == &["CHANUNAVAIL"])
    );
    assert!(control.channel(&parent.id).is_ok());
    Ok(())
}

#[test]
fn generated_children_cannot_replace_an_existing_user_selected_identifier(
) -> Result<(), ControlError> {
    let mut control = Control::new(6);
    control.subscribe("calls")?;
    let reserved = ChannelId::try_from("dial-0000000000000004".to_owned())?;
    let root = ChannelId::try_from("incoming".to_owned())?;
    for (id, caller) in [(reserved.clone(), "kept"), (root.clone(), "100")] {
        control.incoming(
            IncomingCall {
                id,
                endpoint: "phone".into(),
                caller: caller.into(),
                context: "internal".into(),
                extension: "200".into(),
                target: IncomingTarget::Dialplan,
            },
            "now",
        )?;
    }
    let retained = control.key(&reserved)?;
    let parent = control.key(&root)?;
    let program = Dialplan::parse("[internal]\nexten => 200,1,Dial(PJSIP/alice&PJSIP/bob)\n")
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let started = control.start_dialplan(&parent, program, 0, "now")?;
    assert_eq!(control.key(&reserved)?, retained);
    assert_eq!(control.channel(&reserved)?.caller.number, "kept");
    assert_eq!(started.effects.len(), 2);
    assert!(started
        .effects
        .iter()
        .all(|effect| matches!(effect, Effect::Start { key, .. } if key.id != reserved)));
    control.apply(Action::Hangup(root), "later", 1)?;
    assert_eq!(control.key(&reserved)?, retained);
    Ok(())
}

#[test]
fn all_failed_attempts_resume_once_and_late_failures_do_not_end_the_parent(
) -> Result<(), ControlError> {
    let (mut control, parent, started) = start(4, "")?;
    let children = started
        .effects
        .into_iter()
        .map(|effect| match effect {
            Effect::Start { key, .. } => Ok(key),
            Effect::Dtmf { .. }
            | Effect::Gate { .. }
            | Effect::Answer { .. }
            | Effect::Ring { .. }
            | Effect::Release { .. }
            | Effect::Bridge { .. }
            | Effect::DialBridge { .. } => Err(ControlError::Unsupported),
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (index, child) in children.iter().enumerate() {
        let ended = control.signal_at(
            rusterisk_server::Signal::Terminated {
                key: child.clone(),
                cause: 21,
            },
            "later",
            100 + index as u64,
        )?;
        assert!(ended.events.is_empty());
        assert!(control.channel(&parent.id).is_ok());
    }
    let resumed = control.tick_dialplans(200, "later")?;
    assert!(
        matches!(&resumed.events[1].kind, EventKind::StasisStart { args, .. } if args == &["CHANUNAVAIL"])
    );
    for child in children {
        let late = control.signal_at(
            rusterisk_server::Signal::Terminated {
                key: child,
                cause: 21,
            },
            "later",
            300,
        )?;
        assert!(late.events.is_empty());
    }
    assert!(control.channel(&parent.id).is_ok());
    assert!(control.tick_dialplans(400, "later")?.events.is_empty());
    Ok(())
}

#[test]
fn the_first_answer_cancels_its_peers_and_bridges_after_the_parent_answers(
) -> Result<(), ControlError> {
    for selected in 0..2 {
        let (mut control, parent, started) = start(4, ",1")?;
        let children = started
            .effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::Start { key, .. } = effect {
                    Some(key)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let winner = children[selected].clone();
        let loser = children[1 - selected].clone();
        let answered = control.signal_at(
            rusterisk_server::Signal::Milestone {
                key: winner.clone(),
                milestone: rusterisk_server::Milestone::Answered,
            },
            "later",
            100,
        )?;
        assert_eq!(answered.effects.len(), 2);
        assert!(answered
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == &loser)));
        assert!(answered
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Answer { key } if key == &parent)));
        assert!(control.channel(&loser.id).is_err());
        assert_eq!(
            control.channel(&parent.id)?.state,
            rusterisk_api::ChannelState::Ring
        );
        assert!(control
            .signal_at(
                rusterisk_server::Signal::Milestone {
                    key: winner.clone(),
                    milestone: rusterisk_server::Milestone::Answered
                },
                "later",
                101
            )?
            .effects
            .is_empty());
        let connected = control.signal_at(
            rusterisk_server::Signal::Milestone {
                key: parent.clone(),
                milestone: rusterisk_server::Milestone::Answered,
            },
            "later",
            110,
        )?;
        assert!(
            matches!(&connected.effects[..], [Effect::DialBridge { parent: root, peer: Some(peer) }] if root == &parent && peer == &winner)
        );
        assert!(control
            .signal_at(
                rusterisk_server::Signal::Milestone {
                    key: parent.clone(),
                    milestone: rusterisk_server::Milestone::Answered
                },
                "later",
                111
            )?
            .effects
            .is_empty());
        let late = control.signal_at(
            rusterisk_server::Signal::Milestone {
                key: loser.clone(),
                milestone: rusterisk_server::Milestone::Answered,
            },
            "later",
            120,
        )?;
        assert!(matches!(&late.effects[..], [Effect::Release { key }] if key == &loser));
        assert!(control.tick_dialplans(2000, "later")?.events.is_empty());
        let ended = if selected == 0 {
            control.apply(Action::Hangup(parent.id.clone()), "later", 2000)?
        } else {
            control.signal_at(
                rusterisk_server::Signal::Terminated {
                    key: winner.clone(),
                    cause: 16,
                },
                "later",
                2000,
            )?
        };
        assert_eq!(ended.effects.len(), 3);
        assert!(ended.effects.iter().any(|effect| matches!(effect, Effect::DialBridge { parent: root, peer: None } if root == &parent)));
        assert!(control.channel(&parent.id).is_err());
        assert!(control.channel(&winner.id).is_err());
        assert!(control.tick_dialplans(3000, "later")?.events.is_empty());
    }
    Ok(())
}
