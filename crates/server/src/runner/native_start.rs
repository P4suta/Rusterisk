use super::{Controller, Request, Runner};
use crate::{control::NativeStartCapsule, CallKey, ControlError};
use rusterisk_core::SettingsGeneration;
use tokio::sync::oneshot;

#[cfg(test)]
mod tests;

impl Controller {
    pub(crate) async fn take_native_profile(
        &self,
        key: CallKey,
        settings: SettingsGeneration,
    ) -> Result<NativeStartCapsule, ControlError> {
        let mut stopped = self.shutdown_signal();
        if *stopped.borrow() {
            return Err(ControlError::Conflict("Control is stopping"));
        }
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::TakeNativeProfile {
                key,
                settings,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        tokio::select! {
            biased;
            _changed = stopped.changed() => Err(ControlError::Conflict("Control is stopping")),
            result = self.receive(response) => result,
        }
    }
}

impl Runner {
    pub(super) fn native_profile_request(
        &mut self,
        key: CallKey,
        settings: SettingsGeneration,
        reply: oneshot::Sender<Result<NativeStartCapsule, ControlError>>,
    ) -> Result<(), ControlError> {
        if reply.is_closed() {
            return Ok(());
        }
        let result = self.control.take_native_profile(&key, settings);
        self.deliver_native_profile(key, settings, result, reply)
    }

    pub(super) fn deliver_native_profile(
        &mut self,
        key: CallKey,
        settings: SettingsGeneration,
        result: Result<NativeStartCapsule, ControlError>,
        reply: oneshot::Sender<Result<NativeStartCapsule, ControlError>>,
    ) -> Result<(), ControlError> {
        match reply.send(result) {
            Ok(()) => Ok(()),
            Err(Ok(capsule)) => self.control.restore_native_profile(&key, settings, capsule),
            Err(Err(_)) => {
                tracing::debug!("native profile requester closed after rejection");
                Ok(())
            }
        }
    }
}
