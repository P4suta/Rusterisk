use crate::{
    CallKey, ControlError, Controller, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "",
        "",
        "",
    )
    .map_err(failure)
}

fn call(name: &str) -> Result<IncomingCall, ControlError> {
    Ok(IncomingCall {
        id: ChannelId::try_from(name.to_owned())?,
        endpoint: "phone".into(),
        caller: "100".into(),
        context: "internal".into(),
        extension: "100".into(),
        target: IncomingTarget::Stasis {
            application: "app".into(),
            arguments: Vec::new(),
        },
    })
}

// The engine reports a hangup and a new invitation in one batch.
// The composed turn applies the release only after the batch.
#[test]
fn an_invitation_after_a_hangup_in_one_batch_is_admitted_before_the_release_applies(
) -> Result<(), ControlError> {
    let (_controller, runner, _effects) = Controller::new(configuration()?, 4);
    let mut runner = runner.compose();
    runner.control.subscribe("app")?;
    let ended = runner.incoming_call(call("ended")?, 0)??;
    runner.apply_signal(
        Signal::Milestone {
            key: ended.clone(),
            milestone: Milestone::Answered,
        },
        0,
    )?;
    while runner.take_effect().is_some() {}
    runner.apply_signal(
        Signal::Terminated {
            key: ended.clone(),
            cause: 16,
        },
        1,
    )?;
    let admitted: CallKey = runner.incoming_call(call("admitted")?, 1)??;
    assert_ne!(admitted, ended);
    let mut released = false;
    while let Some(effect) = runner.take_effect() {
        released |= matches!(effect, Effect::Release { key } if key == ended);
    }
    assert!(released);
    Ok(())
}

fn waiting_configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "",
        "",
        "[internal]\nexten => 200,1,Wait(0.25)\nsame => n,Hangup()\n",
    )
    .map_err(failure)
}

// A dialplan step falls due in the same turn as an effect that the composed task applies later.
#[test]
fn a_dialplan_step_falls_due_while_the_effects_of_its_turn_still_wait() -> Result<(), ControlError>
{
    let (_controller, runner, _effects) = Controller::new(waiting_configuration()?, 4);
    let mut runner = runner.compose();
    runner.control.subscribe("app")?;
    let waiting = runner.incoming_call(
        IncomingCall {
            target: IncomingTarget::Dialplan,
            extension: "200".into(),
            ..call("waiting")?
        },
        0,
    )??;
    let ended = runner.incoming_call(call("ended")?, 0)??;
    runner.apply_signal(
        Signal::Milestone {
            key: ended.clone(),
            milestone: Milestone::Answered,
        },
        0,
    )?;
    while runner.take_effect().is_some() {}
    runner.apply_signal(
        Signal::Terminated {
            key: ended,
            cause: 16,
        },
        250,
    )?;
    assert!(!runner.pending_effects.is_empty());
    runner.settle(250)?;
    let mut released = false;
    while let Some(effect) = runner.take_effect() {
        released |= matches!(effect, Effect::Release { key } if key == waiting);
    }
    assert!(released, "the dialplan step waited for the queued effects");
    Ok(())
}
