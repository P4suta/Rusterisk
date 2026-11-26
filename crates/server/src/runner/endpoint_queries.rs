use super::Runner;
use crate::{ControlError, Signal};
use rusterisk_core::{ContactPresence, SettingsGeneration};

impl Runner {
    // Records the contact presence of an endpoint in the current settings generation.
    // The outer error ends the composed task, and the inner error refuses a stale observation.
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
