use super::CallLease;
use crate::{Action, CallKey, ControlError, Controller, Effect, Runner};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use tokio::sync::{mpsc, oneshot};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

struct Fixture {
    controller: Controller,
    runner: Runner,
    effects: mpsc::Receiver<Effect>,
    key: CallKey,
}

fn originate(runner: &mut Runner, id: ChannelId) -> Result<CallKey, ControlError> {
    let mut outcome = runner.control.apply(
        Action::Originate {
            id: id.clone(),
            endpoint: "PJSIP/phone".into(),
            app: "app".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "2026-11-16T00:00:00.000+0000",
        0,
    )?;
    let key = runner.control.key(&id)?;
    runner.publish(&mut outcome)?;
    Ok(key)
}

fn fixture() -> Result<Fixture, ControlError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=password\n[static]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=static\nallow=ulaw\n", "", "").map_err(failure)?;
    let (controller, mut runner, effects) = Controller::new(config, 64);
    runner.control.subscribe("app")?;
    let key = originate(&mut runner, ChannelId::try_from("lease-call".to_owned())?)?;
    Ok(Fixture {
        controller,
        runner,
        effects,
        key,
    })
}

#[tokio::test]
async fn native_setup_lifetimes_reject_stale_generations_and_close_with_control(
) -> Result<(), ControlError> {
    let Fixture {
        controller,
        runner,
        effects,
        key,
    } = fixture()?;
    let task = tokio::spawn(runner.run());
    let result: Result<CallLease, ControlError> = async {
        let lease = controller.call_lifetime(key.clone()).await?;
        assert!(!lease.is_released());
        assert!(controller.call_lifetime(key.clone()).await.is_err());
        assert!(controller
            .call_lifetime(CallKey {
                generation: key.generation + 1,
                ..key.clone()
            })
            .await
            .is_err());
        let credentials = || crate::Credentials::new("app".into(), "test".into());
        controller
            .execute(credentials()?, None, Action::Hangup(key.id.clone()))
            .await?;
        assert!(lease.is_released());
        controller
            .execute(
                credentials()?,
                None,
                Action::Originate {
                    id: key.id.clone(),
                    endpoint: "PJSIP/phone".into(),
                    app: "app".into(),
                    args: Vec::new(),
                    caller: "100".into(),
                    timeout: 30,
                },
            )
            .await?;
        assert!(controller.call_lifetime(key.clone()).await.is_err());
        let replacement = controller
            .call_lifetime(CallKey {
                generation: key.generation + 1,
                ..key
            })
            .await?;
        assert!(!replacement.is_released());
        assert!(lease.is_released());
        Ok(replacement)
    }
    .await;
    controller.shutdown();
    task.await.map_err(failure)??;
    drop(effects);
    assert!(result?.is_released());
    Ok(())
}

#[tokio::test]
async fn control_cleanup_closes_the_lease_before_a_pending_release_effect_is_delivered(
) -> Result<(), ControlError> {
    let mut fixture = fixture()?;
    let (lifetime, receiver) = mpsc::channel(1);
    let (reply, receipt) = oneshot::channel();
    fixture
        .runner
        .claim_call_lifetime(fixture.key.clone(), receiver, reply);
    receipt.await.map_err(failure)??;
    let lease = CallLease { lifetime };
    for _ in 0..fixture.runner.effects.capacity() {
        fixture
            .runner
            .effects
            .try_send(Effect::Gate {
                key: fixture.key.clone(),
                gate: rusterisk_core::AudioGate::Closed,
                generated_audio_gate: rusterisk_core::AudioGate::Closed,
            })
            .map_err(failure)?;
    }
    let mut outcome = fixture.runner.control.apply(
        Action::Hangup(fixture.key.id.clone()),
        "2026-11-16T00:00:00.000+0000",
        1,
    )?;
    fixture.runner.publish(&mut outcome)?;
    assert!(lease.is_released());
    assert!(fixture
        .runner
        .pending_effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key } if *key == fixture.key)));
    drop(fixture.effects);
    Ok(())
}
