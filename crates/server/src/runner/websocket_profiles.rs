use super::Runner;
use crate::control::WebSocketStartCapsule;
use crate::signaling::PreparedWebSocketProfiles;
use crate::{CallKey, ControlError};
use rusterisk_core::SettingsGeneration;

impl Runner {
    pub(crate) fn with_websocket_profiles(
        mut self,
        profiles: PreparedWebSocketProfiles,
    ) -> Result<Self, ControlError> {
        self.control.bind_websocket_profiles(profiles)?;
        Ok(self)
    }

    // Hands the WebSocket profile that the call admitted with to the task that connects it.
    pub(crate) fn websocket_profile(
        &mut self,
        key: &CallKey,
        settings: SettingsGeneration,
    ) -> Result<WebSocketStartCapsule, ControlError> {
        self.control.take_websocket_profile(key, settings)
    }
}
