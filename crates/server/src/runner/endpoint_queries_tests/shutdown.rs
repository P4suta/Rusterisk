use super::configuration;
use crate::{ControlError, Controller};
use rusterisk_core::{ContactPresence, SettingsGeneration};
use std::task::Poll;

async fn waiting_observation(
    controller: &Controller,
    generation: SettingsGeneration,
) -> Result<(), ControlError> {
    let reply = controller.observe_endpoint(generation, "phone".into(), ContactPresence::Present);
    tokio::pin!(reply);
    if let Poll::Ready(result) = futures_util::poll!(reply.as_mut()) {
        return Err(ControlError::Adapter(format!(
            "Endpoint observation completed without a running receiver: {result:?}"
        )));
    }
    controller.shutdown();
    match futures_util::poll!(reply.as_mut()) {
        Poll::Ready(result) => {
            if result
                .as_ref()
                .is_err_and(|error| *error == ControlError::Conflict("Control is stopping"))
            {
                Ok(())
            } else {
                Err(ControlError::Adapter(format!(
                    "Endpoint observation lost the shutdown boundary: {result:?}"
                )))
            }
        }
        Poll::Pending => Err(ControlError::Invalid(
            "Endpoint observation kept waiting after shutdown",
        )),
    }
}

#[tokio::test]
async fn a_waiting_endpoint_observation_observes_shutdown_without_waiting_for_its_reply(
) -> Result<(), ControlError> {
    let (controller, runner, effects) = Controller::new(configuration()?, 4);
    let generation = runner.configuration_generation();
    let result = waiting_observation(&controller, generation).await;
    drop((runner, effects));
    result
}
