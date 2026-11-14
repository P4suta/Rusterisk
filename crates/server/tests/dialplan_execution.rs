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
