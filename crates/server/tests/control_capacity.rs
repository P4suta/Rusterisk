use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{Action, ControlError, Controller, Credentials, IncomingCall};

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
                app: "app".into(),
                args: Vec::new(),
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
