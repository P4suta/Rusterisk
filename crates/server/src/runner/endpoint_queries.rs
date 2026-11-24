use super::{Controller, Request, Runner};
use crate::{ControlError, Signal};
use rusterisk_core::{ContactPresence, SettingsGeneration};
use tokio::sync::oneshot;

impl Controller {
    pub(crate) async fn observe_endpoint(
        &self,
        settings: SettingsGeneration,
        endpoint: String,
        presence: ContactPresence,
    ) -> Result<(), ControlError> {
        let mut stopped = self.shutdown_signal();
        if *stopped.borrow() {
            return Err(ControlError::Conflict("Control is stopping"));
        }
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::ObserveEndpoint {
                settings,
                endpoint,
                presence,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        tokio::select! {
            biased;
            _changed = stopped.changed() => Err(ControlError::Conflict("Control is stopping")),
            result = self.receive(receiver) => result,
        }
    }
}

impl Runner {
    pub(super) fn endpoint_observation_request(
        &mut self,
        settings: SettingsGeneration,
        endpoint: String,
        presence: ContactPresence,
        reply: oneshot::Sender<Result<(), ControlError>>,
        now: u64,
    ) -> Result<(), ControlError> {
        if reply.is_closed() {
            return Ok(());
        }
        let key = if self.configuration_generation() != settings {
            Err(ControlError::Conflict(
                "Endpoint settings generation changed",
            ))
        } else {
            self.control.endpoint_key(&endpoint)
        };
        let key = match key {
            Ok(key) => key,
            Err(error) => {
                if reply.send(Err(error)).is_err() {
                    tracing::debug!("Endpoint observer disconnected");
                }
                return Ok(());
            }
        };
        let result = self.apply_signal(Signal::EndpointContact { key, presence }, now);
        if reply.send(result.clone()).is_err() {
            tracing::debug!("Endpoint observer disconnected");
        }
        result
    }
}
