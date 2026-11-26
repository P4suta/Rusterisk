use crate::{
    Action, CallKey, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget,
    Milestone, Signal,
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

// The control loop that runs alone hands a turn's effects to its channel before the next input.
// A request behind a full channel waits for room, as it waits for a composed turn to end.
#[tokio::test]
async fn a_control_loop_alone_delivers_waiting_effects_before_the_next_request(
) -> Result<(), ControlError> {
    let (controller, mut runner, mut effects) = Controller::new(configuration()?, 4);
    let filler = CallKey {
        id: ChannelId::try_from("filler".to_owned())?,
        generation: 1,
    };
    for _ in 0..effects.max_capacity() {
        runner
            .effect_sender()?
            .try_send(Effect::Release {
                key: filler.clone(),
            })
            .map_err(failure)?;
    }
    let waiting = CallKey {
        id: ChannelId::try_from("waiting".to_owned())?,
        generation: 2,
    };
    runner.pending_effects.push_back(Effect::Release {
        key: waiting.clone(),
    });
    let requested = controller.execute(
        Credentials::new("app".into(), "test".into())?,
        None,
        Action::CreateBridge {
            id: ChannelId::try_from("held".to_owned())?,
            name: "held".into(),
        },
    );
    tokio::pin!(requested);
    assert!(futures_util::poll!(requested.as_mut()).is_pending());
    assert_eq!(runner.requests.len(), 1);

    let running = runner.run();
    tokio::pin!(running);
    assert!(futures_util::poll!(running.as_mut()).is_pending());
    assert!(
        futures_util::poll!(requested.as_mut()).is_pending(),
        "the request completed while the release was still waiting"
    );
    for _ in 0..effects.max_capacity() {
        let effect = effects.try_recv().map_err(failure)?;
        assert!(matches!(effect, Effect::Release { key } if key == filler));
    }
    assert!(futures_util::poll!(running.as_mut()).is_pending());
    let effect = effects.try_recv().map_err(failure)?;
    assert!(matches!(effect, Effect::Release { key } if key == waiting));
    let created = futures_util::poll!(requested.as_mut());
    assert!(
        matches!(created, std::task::Poll::Ready(Ok(_))),
        "{created:?}"
    );

    controller.shutdown();
    running.await
}

// A caller that keeps the effect receiver without reading it still sees the loop stop.
#[tokio::test(start_paused = true)]
async fn a_control_loop_alone_stops_while_its_effects_wait_for_a_reader() -> Result<(), ControlError>
{
    let (controller, mut runner, _effects) = Controller::new(configuration()?, 4);
    let filler = CallKey {
        id: ChannelId::try_from("filler".to_owned())?,
        generation: 1,
    };
    while runner
        .effect_sender()?
        .try_send(Effect::Release {
            key: filler.clone(),
        })
        .is_ok()
    {}
    runner
        .pending_effects
        .push_back(Effect::Release { key: filler });
    let running = tokio::spawn(runner.run());
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    controller.shutdown();
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(10), running)
        .await
        .map_err(|_| ControlError::Invalid("The control loop kept waiting for a reader"))?
        .map_err(failure)?;
    assert!(
        matches!(stopped, Err(ControlError::Adapter(_))),
        "{stopped:?}"
    );
    Ok(())
}
