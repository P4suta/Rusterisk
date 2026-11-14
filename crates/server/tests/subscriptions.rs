use rusterisk_api::{ChannelId, EventKind};
use rusterisk_config::Config;
use rusterisk_server::{CallKey, ControlError, Controller, Credentials, Effect, Milestone, Signal};
use std::time::Duration;

fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

#[tokio::test]
async fn replaced_connections_keep_their_admission_slot_until_the_receiver_is_dropped(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let mut subscriptions = Vec::with_capacity(64);
    for _ in 0..64 {
        subscriptions.push(
            controller
                .subscribe(credentials()?, None, "app".into())
                .await?,
        );
    }
    let overflow = controller
        .subscribe(credentials()?, None, "app".into())
        .await;
    assert!(matches!(overflow, Err(ControlError::Capacity)));
    drop(subscriptions.remove(0));
    let replacement = controller
        .subscribe(credentials()?, None, "app".into())
        .await?;
    drop(replacement);
    drop(subscriptions);
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn replacement_and_ingestion_do_not_wait_for_an_event_consumer() -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let mut first = controller
        .subscribe(credentials()?, None, "app".into())
        .await?;
    let second = controller
        .subscribe(credentials()?, None, "app".into())
        .await?;
    let replaced = tokio::time::timeout(Duration::from_secs(1), first.events.recv())
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .ok_or(ControlError::Capacity)?;
    assert!(matches!(replaced.kind, EventKind::ApplicationReplaced {}));
    drop(first);
    let key = CallKey {
        id: ChannelId::try_from("expired".to_owned())?,
        generation: 1,
    };
    controller
        .signal(Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        })
        .await?;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), effects.recv())
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?,
        Some(Effect::Release { key: released }) if released == key
    ));
    assert!(!second.events.is_closed());
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}
