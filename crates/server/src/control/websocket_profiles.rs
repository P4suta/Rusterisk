use super::{CallKey, ChannelEntry, Control, ControlError};
use crate::signaling::{PreparedWebSocketClient, PreparedWebSocketProfiles};
use rusterisk_config::WebSocketClient;
use rusterisk_core::SettingsGeneration;

pub(crate) struct WebSocketStartCapsule {
    key: CallKey,
    generation: SettingsGeneration,
    profile: Result<PreparedWebSocketClient, ControlError>,
}

impl WebSocketStartCapsule {
    fn matches(&self, key: &CallKey, generation: SettingsGeneration) -> bool {
        self.key == *key && self.generation == generation
    }

    pub(crate) fn into_profile(self) -> Result<PreparedWebSocketClient, ControlError> {
        self.profile
    }
}

impl Control {
    pub(crate) fn bind_websocket_profiles(
        &mut self,
        profiles: PreparedWebSocketProfiles,
    ) -> Result<(), ControlError> {
        let current = self.settings.get(self.settings.current())?;
        if current
            .websocket_profiles
            .as_ref()
            .is_some_and(|current| current.same(&profiles))
        {
            return Ok(());
        }
        let mut replacement = current.clone();
        replacement.websocket_profiles = Some(profiles);
        self.publish_call_settings(replacement)?;
        Ok(())
    }

    pub(super) fn websocket_capsule(
        &self,
        key: &CallKey,
        generation: SettingsGeneration,
        client: &WebSocketClient,
    ) -> Result<WebSocketStartCapsule, ControlError> {
        let settings = self.settings.get(generation)?;
        Ok(WebSocketStartCapsule {
            key: key.clone(),
            generation,
            profile: settings
                .websocket_profiles
                .as_ref()
                .ok_or(ControlError::Invalid(
                    "WebSocket profiles are not configured",
                ))
                .and_then(|profiles| profiles.capture(client)),
        })
    }

    fn websocket_entry(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
    ) -> Result<&mut ChannelEntry, ControlError> {
        self.settings.get(generation)?;
        self.channels
            .get_mut(&key.id)
            .filter(|entry| {
                entry.call.generation() == key.generation
                    && entry.settings.generation() == generation
            })
            .ok_or(ControlError::Conflict("WebSocket start generation changed"))
    }

    pub(crate) fn take_websocket_profile(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
    ) -> Result<WebSocketStartCapsule, ControlError> {
        let entry = self.websocket_entry(key, generation)?;
        if !entry
            .pending_websocket
            .as_ref()
            .is_some_and(|capsule| capsule.matches(key, generation))
        {
            return Err(ControlError::Conflict("WebSocket profile already claimed"));
        }
        entry
            .pending_websocket
            .take()
            .ok_or(ControlError::Conflict("WebSocket profile already claimed"))
    }

    pub(crate) fn restore_websocket_profile(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
        capsule: WebSocketStartCapsule,
    ) -> Result<(), ControlError> {
        if !capsule.matches(key, generation) {
            return Err(ControlError::Conflict(
                "WebSocket capsule generation changed",
            ));
        }
        let entry = self.websocket_entry(key, generation)?;
        if entry.pending_websocket.is_some() {
            return Err(ControlError::Conflict("WebSocket profile already pending"));
        }
        entry.pending_websocket = Some(capsule);
        Ok(())
    }
}
