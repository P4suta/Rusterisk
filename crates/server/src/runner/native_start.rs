use super::Runner;
use crate::{control::NativeStartCapsule, CallKey, ControlError};
use rusterisk_core::SettingsGeneration;

impl Runner {
    // Hands the start profile that the call admitted with to the task that starts it.
    pub(crate) fn native_profile(
        &mut self,
        key: &CallKey,
        settings: SettingsGeneration,
    ) -> Result<NativeStartCapsule, ControlError> {
        self.control.take_native_profile(key, settings)
    }
}
