use super::{receive, Controller, Request, Runner};
use crate::control::WebSocketStartCapsule;
use crate::signaling::{PreparedWebSocketClient, PreparedWebSocketProfiles};
use crate::{CallKey, ControlError};
use rusterisk_core::SettingsGeneration;
use tokio::sync::oneshot;

impl Controller {
    pub(crate) fn bind_websocket_profiles(
        &self,
        profiles: PreparedWebSocketProfiles,
    ) -> Result<(), ControlError> {
        self.requests
            .try_send(Request::BindWebSocketProfiles(profiles))
            .map_err(|_| ControlError::Capacity)
    }

    pub(crate) async fn take_websocket_profile(
        &self,
        key: CallKey,
        settings: SettingsGeneration,
    ) -> Result<PreparedWebSocketClient, ControlError> {
        let mut stopped = self.shutdown_signal();
        if *stopped.borrow() {
            return Err(ControlError::Conflict("Control is stopping"));
        }
        let (reply, receiver) = oneshot::channel();
        self.requests
            .try_send(Request::TakeWebSocketProfile {
                key,
                settings,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        tokio::select! {
            biased;
            _changed = stopped.changed() => Err(ControlError::Conflict("Control is stopping")),
            result = receive(receiver) => result?.into_profile(),
        }
    }
}

impl Runner {
    pub(crate) fn with_websocket_profiles(
        mut self,
        profiles: PreparedWebSocketProfiles,
    ) -> Result<Self, ControlError> {
        self.control.bind_websocket_profiles(profiles)?;
        Ok(self)
    }

    pub(super) fn websocket_profile_request(
        &mut self,
        key: CallKey,
        settings: SettingsGeneration,
        reply: oneshot::Sender<Result<WebSocketStartCapsule, ControlError>>,
    ) -> Result<(), ControlError> {
        if reply.is_closed() {
            return Ok(());
        }
        let result = self.control.take_websocket_profile(&key, settings);
        match reply.send(result) {
            Ok(()) => Ok(()),
            Err(Ok(capsule)) => self
                .control
                .restore_websocket_profile(&key, settings, capsule),
            Err(Err(_)) => {
                tracing::debug!("WebSocket profile requester disconnected after rejection");
                Ok(())
            }
        }
    }
}
