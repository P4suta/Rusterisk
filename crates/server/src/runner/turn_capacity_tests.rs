use super::Runner;
use crate::{
    Action, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget, Milestone,
    Signal,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use std::future::Future;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)
}

fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

// Serves requests in one composed turn, which keeps every effect until the test takes it.
// The public interface ends a turn after each request, so only this harness fills a turn.
async fn serve_until<T>(
    runner: &mut Runner,
    future: impl Future<Output = T>,
) -> Result<T, ControlError> {
    let mut future = std::pin::pin!(future);
    loop {
        tokio::select! {
            biased;
            output = &mut future => return Ok(output),
            input = runner.next() => { runner.handle(input, 0)?; }
        }
    }
}

fn incoming(name: &str, application: &str) -> Result<IncomingCall, ControlError> {
    Ok(IncomingCall {
        id: ChannelId::try_from(name.to_owned())?,
        endpoint: "phone".into(),
        caller: "100".into(),
        context: "internal".into(),
        extension: "200".into(),
        target: IncomingTarget::Stasis {
            application: application.into(),
            arguments: Vec::new(),
        },
    })
}

// Rings until control refuses the next ring for lack of room in the turn, and returns the count.
async fn ring_until_refused(
    runner: &mut Runner,
    controller: &Controller,
    id: &ChannelId,
) -> Result<usize, ControlError> {
    for rung in 0..1024 {
        match serve_until(
            runner,
            controller.execute(credentials()?, None, Action::Ring(id.clone())),
        )
        .await?
        {
            Ok(_) => {}
            Err(ControlError::Capacity) => return Ok(rung),
            Err(error) => return Err(error),
        }
    }
    Err(ControlError::Invalid("Control never refused a ring"))
}

// An application that stops reading its events ends its call in a turn that ordinary work filled.
#[tokio::test]
async fn event_overflow_preserves_terminal_cleanup_when_the_turn_is_full(
) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(configuration()?, 4);
    let mut runner = runner.compose();
    let _subscription = serve_until(
        &mut runner,
        controller.subscribe(credentials()?, None, "app".into()),
    )
    .await??;
    let call = incoming("incoming", "app")?;
    let id = call.id.clone();
    let key = serve_until(&mut runner, controller.incoming(call)).await??;
    let rung = ring_until_refused(&mut runner, &controller, &id).await?;
    for index in 0..63 {
        serve_until(
            &mut runner,
            controller.execute(
                credentials()?,
                None,
                Action::SetVariable {
                    channel: id.clone(),
                    variable: "VALUE".into(),
                    value: Some(index.to_string()),
                },
            ),
        )
        .await??;
    }
    assert!(matches!(
        serve_until(
            &mut runner,
            controller.execute(credentials()?, None, Action::GetChannel(id))
        )
        .await?,
        Err(ControlError::NotFound(_))
    ));
    // The cleanup spilled into the terminal reserve, so new work waits for the turn to end.
    let _replacement = serve_until(
        &mut runner,
        controller.subscribe(credentials()?, None, "replacement".into()),
    )
    .await??;
    let next = incoming("replacement", "replacement")?;
    assert!(matches!(
        serve_until(&mut runner, controller.incoming(next.clone())).await?,
        Err(ControlError::Capacity)
    ));
    assert!(matches!(
        serve_until(
            &mut runner,
            controller.execute(
                credentials()?,
                None,
                Action::CreateBridge {
                    id: ChannelId::try_from("deferred".to_owned())?,
                    name: "Deferred".into(),
                },
            )
        )
        .await?,
        Err(ControlError::Capacity)
    ));
    assert_eq!(rung, super::TURN_EFFECTS);
    for _ in 0..rung {
        assert!(matches!(runner.take_effect(), Some(Effect::Ring { .. })));
    }
    assert!(
        matches!(runner.take_effect(), Some(Effect::Release { key: ended }) if ended == key),
        "the ended call kept its media"
    );
    let admitted = serve_until(&mut runner, controller.incoming(next)).await??;
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::Hangup(admitted.id)),
    )
    .await??;
    assert!(runner.take_effect().is_some());
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::ListChannels),
    )
    .await??;
    Ok(())
}

// Ordinary work fills the turn, and control refuses the next request without ending the call.
#[tokio::test]
async fn a_full_turn_refuses_a_request_without_stopping_existing_calls() -> Result<(), ControlError>
{
    let (controller, runner, _effects) = Controller::new(configuration()?, 4);
    let mut runner = runner.compose();
    let _subscription = serve_until(
        &mut runner,
        controller.subscribe(credentials()?, None, "app".into()),
    )
    .await??;
    let call = incoming("incoming", "app")?;
    let id = call.id.clone();
    serve_until(&mut runner, controller.incoming(call)).await??;
    assert_eq!(
        ring_until_refused(&mut runner, &controller, &id).await?,
        super::TURN_EFFECTS
    );
    let channel = serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::GetChannel(id.clone())),
    )
    .await??;
    assert!(channel.body.is_some());
    assert!(!controller.is_stopping());
    assert!(runner.take_effect().is_some());
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::Ring(id)),
    )
    .await??;
    Ok(())
}

fn dialing_configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "[internal]\nexten => 200,1,Dial(PJSIP/alice&PJSIP/bob)\n",
    )
    .map_err(failure)
}

// A hangup that cancels a dial needs room for every child, and control checks it before any change.
#[tokio::test]
async fn dial_cancellation_checks_capacity_for_every_owned_child_before_changing_state(
) -> Result<(), ControlError> {
    check_dial_capacity(false).await?;
    check_dial_capacity(true).await
}

async fn check_dial_capacity(connected: bool) -> Result<(), ControlError> {
    let (controller, runner, _effects) = Controller::new(dialing_configuration()?, 4);
    let mut runner = runner.compose();
    let _subscription = serve_until(
        &mut runner,
        controller.subscribe(credentials()?, None, "app".into()),
    )
    .await??;
    let ringing = incoming("ringing", "app")?;
    let ringing_id = ringing.id.clone();
    serve_until(&mut runner, controller.incoming(ringing)).await??;
    let parent = IncomingCall {
        target: IncomingTarget::Dialplan,
        ..incoming("parent", "app")?
    };
    let parent_id = parent.id.clone();
    let parent_key = serve_until(&mut runner, controller.incoming(parent)).await??;
    runner.settle(0)?;
    let mut children = Vec::new();
    for _ in 0..2 {
        let Some(Effect::Start { key, .. }) = runner.take_effect() else {
            return Err(ControlError::Unsupported);
        };
        children.push(key);
    }
    let cancelled = if connected {
        runner.apply_signal(
            Signal::Milestone {
                key: children[0].clone(),
                milestone: Milestone::Answered,
            },
            0,
        )?;
        runner.settle(0)?;
        for _ in 0..2 {
            runner.take_effect().ok_or(ControlError::Capacity)?;
        }
        runner.apply_signal(
            Signal::Milestone {
                key: parent_key,
                milestone: Milestone::Answered,
            },
            0,
        )?;
        runner.settle(0)?;
        assert!(matches!(
            runner.take_effect(),
            Some(Effect::DialBridge { peer: Some(_), .. })
        ));
        children[0].id.clone()
    } else {
        parent_id.clone()
    };
    assert!(runner.take_effect().is_none());
    let filled = super::TURN_EFFECTS - 2;
    for _ in 0..filled {
        serve_until(
            &mut runner,
            controller.execute(credentials()?, None, Action::Ring(ringing_id.clone())),
        )
        .await??;
    }
    assert!(matches!(
        serve_until(
            &mut runner,
            controller.execute(credentials()?, None, Action::Hangup(cancelled.clone()))
        )
        .await?,
        Err(ControlError::Capacity)
    ));
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::GetChannel(parent_id)),
    )
    .await??;
    runner.take_effect().ok_or(ControlError::Capacity)?;
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::Hangup(cancelled)),
    )
    .await??;
    for _ in 1..filled {
        assert!(matches!(runner.take_effect(), Some(Effect::Ring { .. })));
    }
    let mut releases = 0;
    let mut removed = 0;
    for _ in 0..3 {
        match runner.take_effect() {
            Some(Effect::Release { .. }) => releases += 1,
            Some(Effect::DialBridge { peer: None, .. }) => removed += 1,
            other => return Err(failure(format!("unexpected effect {other:?}"))),
        }
    }
    assert_eq!(releases, if connected { 2 } else { 3 });
    assert_eq!(removed, usize::from(connected));
    serve_until(
        &mut runner,
        controller.execute(credentials()?, None, Action::Hangup(ringing_id)),
    )
    .await??;
    runner.take_effect().ok_or(ControlError::Capacity)?;
    Ok(())
}
