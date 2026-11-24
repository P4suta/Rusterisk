use rusterisk_api::ChannelId;
use rusterisk_config::Dialplan;
use rusterisk_core::Wake;
use rusterisk_server::{Action, CallKey, Control, ControlError, IncomingCall, IncomingTarget};

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

fn program(source: &str) -> Result<Dialplan, ControlError> {
    Dialplan::parse(source).map_err(failure)
}

#[test]
fn a_control_without_channels_needs_no_wake() {
    let control = Control::new(4);
    assert_eq!(control.expiry_wake(), Wake::Idle);
    assert_eq!(control.dialplan_wake(), Wake::Idle);
}

#[test]
fn an_originated_channel_needs_a_wake_at_its_timeout_and_none_after_it() -> Result<(), ControlError>
{
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.apply(
        Action::Originate {
            id: ChannelId::try_from("timed".to_owned())?,
            endpoint: "PJSIP/phone".into(),
            app: "calls".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "now",
        250,
    )?;
    assert_eq!(control.expiry_wake(), Wake::At(30_250));
    assert!(control.expire(30_249, "early")?.effects.is_empty());
    assert_eq!(control.expiry_wake(), Wake::At(30_250));
    assert!(!control.expire(30_250, "due")?.effects.is_empty());
    assert_eq!(control.expiry_wake(), Wake::Idle);
    Ok(())
}

#[test]
fn a_waiting_dialplan_needs_a_wake_at_its_instant_and_none_while_it_awaits_stasis(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let key = incoming(&mut control, "waiting")?;
    control.start_dialplan(
        &key,
        program("[internal]\nexten => 200,1,Wait(0.125)\nsame => n,Stasis(calls)\n")?,
        50,
        "now",
    )?;
    assert_eq!(control.dialplan_wake(), Wake::At(175));
    assert!(control.tick_dialplans(174, "early")?.events.is_empty());
    assert_eq!(control.dialplan_wake(), Wake::At(175));
    assert!(!control.tick_dialplans(175, "due")?.events.is_empty());
    assert_eq!(control.dialplan_wake(), Wake::Idle);
    Ok(())
}

fn looping() -> Result<Dialplan, ControlError> {
    program("[internal]\nexten => 200,1,NoOp(again)\nsame => n,Goto(1)\n")
}

#[test]
fn a_dialplan_that_spends_its_turn_budget_needs_the_next_turn() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let key = incoming(&mut control, "looping")?;
    control.start_dialplan(&key, looping()?, 0, "now")?;
    assert_eq!(control.dialplan_wake(), Wake::Now);
    Ok(())
}

#[test]
fn a_dialplan_that_keeps_spending_its_budget_is_paced_after_four_turns() -> Result<(), ControlError>
{
    let mut control = Control::new(4);
    let key = incoming(&mut control, "looping")?;
    control.start_dialplan(&key, looping()?, 0, "now")?;
    for turn in 1..=2 {
        control.tick_dialplans(turn, "again")?;
        assert_eq!(control.dialplan_wake(), Wake::Now);
    }
    control.tick_dialplans(3, "again")?;
    assert_eq!(control.dialplan_wake(), Wake::At(103));
    let paced = control.channel(&key.id)?.clone();
    control.tick_dialplans(102, "early")?;
    assert_eq!(control.channel(&key.id)?, &paced);
    assert_eq!(control.dialplan_wake(), Wake::At(103));
    control.tick_dialplans(103, "due")?;
    assert_eq!(control.dialplan_wake(), Wake::At(203));
    Ok(())
}
