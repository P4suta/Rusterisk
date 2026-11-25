use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::{Action, ControlError, Controller, Credentials, IncomingCall};
use rusterisk_test_support::Wait;

// The control loop ends each request's turn, so the public interface can't fill one turn.
// The unit tests beside the control loop cover a full turn.
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
    Wait::Ready
        .bounded(task)
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    Ok(())
}
