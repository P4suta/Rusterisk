use rusterisk_api::{ChannelId, EventKind};
use rusterisk_config::Dialplan;
use rusterisk_server::{
    Action, CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn incoming(control: &mut Control, id: &str) -> Result<CallKey, ControlError> {
    let id = ChannelId::try_from(id.to_owned())?;
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
    control.key(&id)
}

#[test]
fn waits_resume_the_starting_program_and_stasis_observes_the_current_priority(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let key = incoming(&mut control, "first")?;
    let program = Dialplan::parse(
        "[internal]\nexten => 200,1,NoOp(begin ${EXTEN})\nsame => n,Wait(1)\nsame => n,Goto(other,300,7)\n[other]\nexten => 300,7,Stasis(calls,${CONTEXT},${EXTEN},${PRIORITY})\nsame => n,Hangup()\n",
    ).map_err(failure)?;
    let started = control.start_dialplan(&key, program, 50, "now")?;
    assert!(started.effects.is_empty());
    assert!(started.events.is_empty());
    assert_eq!(control.channel(&key.id)?.dialplan.app_name, "Wait");
    assert!(control.tick_dialplans(1049, "later")?.events.is_empty());
    let entered = control.tick_dialplans(1050, "later")?;
    assert_eq!(entered.events.len(), 2);
    assert!(
        matches!(&entered.events[0].kind, EventKind::ChannelCreated { channel } if channel.id == key.id)
    );
    assert!(
        matches!(&entered.events[1].kind, EventKind::StasisStart { channel, args } if channel.dialplan.context == "other" && channel.dialplan.exten == "300" && channel.dialplan.priority == 7 && args == &["other", "300", "7"])
    );
    assert!(control.tick_dialplans(5000, "later")?.events.is_empty());
    let ended = control.apply(Action::Hangup(key.id.clone()), "later", 5000)?;
    assert!(matches!(&ended.effects[0], Effect::Release { key: released } if released == &key));
    assert!(control.tick_dialplans(6000, "later")?.effects.is_empty());
    Ok(())
}

#[test]
fn immediate_hangup_and_missing_routes_release_only_their_own_channel() -> Result<(), ControlError>
{
    let mut control = Control::new(4);
    let key = incoming(&mut control, "first")?;
    assert!(matches!(
        control.start_dialplan(
            &key,
            Dialplan::parse("[elsewhere]\nexten => 200,1,Hangup()\n").map_err(failure)?,
            0,
            "now"
        ),
        Err(ControlError::NotFound("Extension not found"))
    ));
    assert!(control.channel(&key.id).is_ok());
    let ended = control.start_dialplan(
        &key,
        Dialplan::parse("[internal]\nexten => 200,1,Hangup()\n").map_err(failure)?,
        0,
        "now",
    )?;
    assert!(matches!(&ended.effects[0], Effect::Release { key: released } if released == &key));
    assert!(matches!(
        control.channel(&key.id),
        Err(ControlError::NotFound(_))
    ));
    let replacement = incoming(&mut control, "first")?;
    assert_ne!(key, replacement);
    assert!(matches!(
        control.start_dialplan(
            &key,
            Dialplan::parse("[internal]\nexten => 200,1,Hangup()\n").map_err(failure)?,
            0,
            "now"
        ),
        Err(ControlError::Conflict(_))
    ));
    assert!(control.channel(&replacement.id).is_ok());
    Ok(())
}

#[test]
fn looping_priorities_yield_to_channel_controls_each_turn() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let key = incoming(&mut control, "loop")?;
    let program = Dialplan::parse("[internal]\nexten => 200,1,NoOp(loop)\nsame => n,Goto(1)\n")
        .map_err(failure)?;
    let started = control.start_dialplan(&key, program, 0, "now")?;
    assert!(started.effects.is_empty());
    for now in [0, 100, 200] {
        assert!(control.tick_dialplans(now, "later")?.effects.is_empty());
        assert!(control.channel(&key.id).is_ok());
    }
    control.apply(Action::Hangup(key.id.clone()), "later", 200)?;
    assert!(control.tick_dialplans(300, "later")?.effects.is_empty());
    Ok(())
}

#[test]
fn assignments_expand_current_values_and_keep_variables_with_their_call() -> Result<(), ControlError>
{
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let first = incoming(&mut control, "first")?;
    let second = incoming(&mut control, "second")?;
    let program = Dialplan::parse(
        "[internal]\nexten => 200,1,Set(X=${CALLERID(num)}-${EXTEN}-${PRIORITY})\nsame => n,Set(CALLERID(name)=Desk ${X})\nsame => n,Set(CALLERID(num)=300)\nsame => n,Stasis(calls,${X},${MISSING},${CALLERID(all)})\n",
    ).map_err(failure)?;
    let started = control.start_dialplan(&first, program, 0, "now")?;
    assert_eq!(started.events.len(), 2);
    assert!(
        matches!(&started.events[1].kind, EventKind::StasisStart { args, .. } if args == &["100-200-1", "", "\"Desk 100-200-1\" <300>"])
    );
    assert_eq!(control.channel(&first.id)?.caller.number, "300");
    assert_eq!(control.channel(&second.id)?.caller.number, "100");
    assert_eq!(control.key(&first.id)?, first);
    let variable = control.apply(
        Action::GetVariable {
            channel: first.id.clone(),
            variable: "X".into(),
        },
        "now",
        0,
    )?;
    assert_eq!(
        variable.body,
        Some(serde_json::json!({"value": "100-200-1"}))
    );
    let refused = control.apply(
        Action::SetVariable {
            channel: second.id,
            variable: "X".into(),
            value: Some("outside".into()),
        },
        "now",
        0,
    );
    assert!(matches!(refused, Err(ControlError::Conflict(_))));
    Ok(())
}

#[test]
fn answer_suspends_until_the_response_and_media_or_its_deadline() -> Result<(), ControlError> {
    for receive_media in [false, true] {
        let mut control = Control::new(4);
        control.subscribe("calls")?;
        let key = incoming(&mut control, "first")?;
        let program = Dialplan::parse(
            "[internal]\nexten => 200,1,Answer()\nsame => n,Answer()\nsame => n,Stasis(calls)\n",
        )
        .map_err(failure)?;
        let started = control.start_dialplan(&key, program, 0, "now")?;
        assert!(
            matches!(&started.effects[..], [Effect::Answer { key: answered }] if answered == &key)
        );
        assert!(started.events.is_empty());
        assert!(control.tick_dialplans(1000, "later")?.events.is_empty());
        control.signal(
            rusterisk_server::Signal::Milestone {
                key: key.clone(),
                milestone: rusterisk_server::Milestone::Answered,
            },
            "later",
        )?;
        assert!(control.tick_dialplans(1000, "later")?.events.is_empty());
        assert!(control.tick_dialplans(1499, "later")?.events.is_empty());
        if receive_media {
            control.signal(
                rusterisk_server::Signal::Milestone {
                    key: key.clone(),
                    milestone: rusterisk_server::Milestone::MediaConnected,
                },
                "later",
            )?;
        }
        let entered = control.tick_dialplans(if receive_media { 1499 } else { 1500 }, "later")?;
        assert!(entered.effects.is_empty());
        assert!(
            matches!(&entered.events[1].kind, EventKind::StasisStart { channel, .. } if channel.state == rusterisk_api::ChannelState::Up && channel.dialplan.priority == 3)
        );
        assert!(control.tick_dialplans(2000, "later")?.events.is_empty());
    }
    Ok(())
}

#[test]
fn hangup_cancels_a_suspended_answer_before_late_milestones() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let key = incoming(&mut control, "first")?;
    control.start_dialplan(
        &key,
        Dialplan::parse("[internal]\nexten => 200,1,Answer()\nsame => n,Stasis(calls)\n")
            .map_err(failure)?,
        0,
        "now",
    )?;
    control.apply(Action::Hangup(key.id.clone()), "later", 1)?;
    let late = control.signal(
        rusterisk_server::Signal::Milestone {
            key: key.clone(),
            milestone: rusterisk_server::Milestone::Answered,
        },
        "later",
    )?;
    assert!(matches!(&late.effects[..], [Effect::Release { key: released }] if released == &key));
    assert!(late.events.is_empty());
    assert!(control.tick_dialplans(2000, "later")?.events.is_empty());
    Ok(())
}

#[test]
fn an_unregistered_stasis_app_sets_failure_and_continues_to_the_following_priority(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let key = incoming(&mut control, "first")?;
    let program = Dialplan::parse("[internal]\nexten => 200,1,Stasis(unregistered)\nsame => n,Stasis(calls,${STASISSTATUS})\n").map_err(failure)?;
    let entered = control.start_dialplan(&key, program, 0, "now")?;
    assert!(entered.effects.is_empty());
    assert!(
        matches!(&entered.events[1].kind, EventKind::StasisStart { args, .. } if args == &["FAILED"])
    );
    assert_eq!(
        control
            .apply(
                Action::GetVariable {
                    channel: key.id.clone(),
                    variable: "STASISSTATUS".into()
                },
                "now",
                0
            )?
            .body,
        Some(serde_json::json!({"value": ""}))
    );
    for index in 0..64 {
        control.apply(
            Action::SetVariable {
                channel: key.id.clone(),
                variable: format!("X{index}"),
                value: Some("kept".into()),
            },
            "now",
            0,
        )?;
    }
    assert!(matches!(
        control.apply(
            Action::SetVariable {
                channel: key.id,
                variable: "OVERFLOW".into(),
                value: Some("refused".into())
            },
            "now",
            0
        ),
        Err(ControlError::Capacity)
    ));
    Ok(())
}
