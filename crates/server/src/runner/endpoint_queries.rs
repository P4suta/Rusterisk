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
        match self.endpoint_observation(settings, endpoint, presence, now) {
            Ok(answer) => {
                if reply.send(answer).is_err() {
                    tracing::debug!("Endpoint observer disconnected");
                }
                Ok(())
            }
            Err(error) => {
                if reply.send(Err(error.clone())).is_err() {
                    tracing::debug!("Endpoint observer disconnected");
                }
                Err(error)
            }
        }
    }

    // Records the contact presence of an endpoint in the current settings generation.
    // The outer error ends the control task, and the inner error refuses a stale observation.
    pub(crate) fn endpoint_observation(
        &mut self,
        settings: SettingsGeneration,
        endpoint: String,
        presence: ContactPresence,
        now: u64,
    ) -> Result<Result<(), ControlError>, ControlError> {
        let key = if self.configuration_generation() != settings {
            Err(ControlError::Conflict(
                "Endpoint settings generation changed",
            ))
        } else {
            self.control.endpoint_key(&endpoint)
        };
        match key {
            Ok(key) => self
                .apply_signal(Signal::EndpointContact { key, presence }, now)
                .map(Ok),
            Err(error) => Ok(Err(error)),
        }
    }
}
