use super::{Command, MediaPort};
use crate::{rtp::Measurements, CallKey, ControlError};
use std::time::Duration;
use tokio::sync::oneshot;

impl MediaPort {
    pub(crate) async fn measurements(
        &self,
        key: CallKey,
    ) -> Result<Option<(String, Measurements)>, ControlError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send(Command::Measurements { key, reply })
            .map_err(ControlError::queue)?;
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .map_err(|_| ControlError::Capacity)?
            .map_err(|_| ControlError::ServiceClosed)
    }
}
