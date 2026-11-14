use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{Action, ControlError, Controller, Credentials, IncomingCall};
use std::time::Duration;

#[tokio::test]
async fn dial_cancellation_checks_capacity_for_every_owned_child_before_changing_state(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "[internal]\nexten => 200,1,Dial(PJSIP/alice&PJSIP/bob)\n",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let result = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _subscription = controller
            .subscribe(credentials.clone(), None, "app".into())
            .await?;
        let ringing = ChannelId::try_from("ringing".to_owned())?;
        let parent = ChannelId::try_from("parent".to_owned())?;
        for (id, target) in [
            (
                ringing.clone(),
                rusterisk_server::IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            ),
            (parent.clone(), rusterisk_server::IncomingTarget::Dialplan),
        ] {
            controller
                .incoming(IncomingCall {
                    id,
                    endpoint: "phone".into(),
                    caller: "100".into(),
                    context: "internal".into(),
                    extension: "200".into(),
                    target,
                })
                .await?;
        }
        for _ in 0..2 {
            assert!(matches!(
                effects.recv().await,
                Some(rusterisk_server::Effect::Start { .. })
            ));
        }
        for _ in 0..126 {
            controller
                .execute(credentials.clone(), None, Action::Ring(ringing.clone()))
                .await?;
        }
        assert!(matches!(
            controller
                .execute(credentials.clone(), None, Action::Hangup(parent.clone()))
                .await,
            Err(ControlError::Capacity)
        ));
        controller
            .execute(
                credentials.clone(),
                None,
                Action::GetChannel(parent.clone()),
            )
            .await?;
        effects.recv().await.ok_or(ControlError::Capacity)?;
        controller
            .execute(credentials.clone(), None, Action::Hangup(parent))
            .await?;
        for _ in 0..125 {
            assert!(matches!(
                effects.recv().await,
                Some(rusterisk_server::Effect::Ring { .. })
            ));
        }
        for _ in 0..3 {
            assert!(matches!(
                effects.recv().await,
                Some(rusterisk_server::Effect::Release { .. })
            ));
        }
        controller
            .execute(credentials, None, Action::Hangup(ringing))
            .await?;
        effects.recv().await.ok_or(ControlError::Capacity)?;
        Ok(())
    }
    .await;
    controller.shutdown();
    while effects.try_recv().is_ok() {}
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}

#[tokio::test]
async fn global_shutdown_cancels_effects_when_the_owned_signaling_scope_closes(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let credentials = Credentials::new("app".into(), "test-password".into())?;
    let _subscription = controller
        .subscribe(credentials.clone(), None, "app".into())
        .await?;
    let id = ChannelId::try_from("incoming".to_owned())?;
    controller
        .incoming(IncomingCall {
            id: id.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: rusterisk_server::IncomingTarget::Stasis {
                application: "app".into(),
                arguments: Vec::new(),
            },
        })
        .await?;
    for _ in 0..128 {
        controller
            .execute(credentials.clone(), None, Action::Ring(id.clone()))
            .await?;
    }
    for index in 0..63 {
        controller
            .execute(
                credentials.clone(),
                None,
                Action::SetVariable {
                    channel: id.clone(),
                    variable: "VALUE".into(),
                    value: Some(index.to_string()),
                },
            )
            .await?;
    }
    controller.shutdown();
    drop(effects);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}

#[tokio::test]
async fn event_overflow_preserves_terminal_cleanup_when_the_effect_queue_is_full(
) -> Result<(), ControlError> {
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
    let result: Result<(), ControlError> = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _subscription = controller
            .subscribe(credentials.clone(), None, "app".into())
            .await?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        let key = controller
            .incoming(IncomingCall {
                id: id.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "200".into(),
                target: rusterisk_server::IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        for _ in 0..128 {
            controller
                .execute(credentials.clone(), None, Action::Ring(id.clone()))
                .await?;
        }
        for index in 0..63 {
            controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::SetVariable {
                        channel: id.clone(),
                        variable: "VALUE".into(),
                        value: Some(index.to_string()),
                    },
                )
                .await?;
        }
        assert!(matches!(
            controller
                .execute(credentials.clone(), None, Action::GetChannel(id))
                .await,
            Err(ControlError::NotFound(_))
        ));
        let _replacement = controller
            .subscribe(credentials.clone(), None, "replacement".into())
            .await?;
        let next = IncomingCall {
            id: ChannelId::try_from("replacement".to_owned())?,
            endpoint: "phone".into(),
            caller: "101".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: rusterisk_server::IncomingTarget::Stasis {
                application: "replacement".into(),
                arguments: Vec::new(),
            },
        };
        assert!(matches!(
            controller.incoming(next.clone()).await,
            Err(ControlError::Capacity)
        ));
        assert!(matches!(
            controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::CreateBridge {
                        id: ChannelId::try_from("deferred".to_owned())?,
                        name: "Deferred".into()
                    }
                )
                .await,
            Err(ControlError::Capacity)
        ));
        for _ in 0..128 {
            let effect = effects.recv().await.ok_or(ControlError::Capacity)?;
            assert!(matches!(effect, rusterisk_server::Effect::Ring { .. }));
        }
        let release = tokio::time::timeout(Duration::from_secs(2), effects.recv())
            .await
            .map_err(|error| ControlError::Adapter(error.to_string()))?
            .ok_or(ControlError::Capacity)?;
        assert!(
            matches!(release, rusterisk_server::Effect::Release { key: ended } if ended == key)
        );
        let admitted = controller.incoming(next).await?;
        controller
            .execute(credentials.clone(), None, Action::Hangup(admitted.id))
            .await?;
        effects.recv().await.ok_or(ControlError::Capacity)?;
        controller
            .execute(credentials, None, Action::ListChannels)
            .await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    while effects.try_recv().is_ok() {}
    let ended = task
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    result?;
    ended
}

#[tokio::test]
async fn a_full_effect_queue_rejects_the_request_without_stopping_existing_calls(
) -> Result<(), ControlError> {
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
    let result: Result<(), ControlError> = async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let _subscription = controller
            .subscribe(credentials.clone(), None, "app".into())
            .await?;
        let id = ChannelId::try_from("incoming".to_owned())?;
        controller
            .incoming(IncomingCall {
                id: id.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "200".into(),
                target: rusterisk_server::IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        for _ in 0..128 {
            controller
                .execute(credentials.clone(), None, Action::Ring(id.clone()))
                .await?;
        }
        assert!(matches!(
            controller
                .execute(credentials.clone(), None, Action::Ring(id.clone()))
                .await,
            Err(ControlError::Capacity)
        ));
        let channel = controller
            .execute(credentials.clone(), None, Action::GetChannel(id.clone()))
            .await?;
        assert!(channel.body.is_some());
        assert!(!controller.is_stopping());
        effects.recv().await.ok_or(ControlError::Capacity)?;
        controller
            .execute(credentials, None, Action::Ring(id))
            .await?;
        while effects.try_recv().is_ok() {}
        Ok(())
    }
    .await;
    controller.shutdown();
    while effects.try_recv().is_ok() {}
    let ended = task
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    result?;
    ended
}
