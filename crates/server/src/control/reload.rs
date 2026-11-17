use super::{CallSettings, Control, ControlError, Endpoints};
use rusterisk_config::{Config, Dialplan, WebSocketConfig};
use rusterisk_core::SettingsCandidate;

pub(super) struct PreparedConfiguration {
    key: SettingsCandidate,
    config: Config,
    endpoints: Endpoints,
    dialplan: Dialplan,
    websocket: WebSocketConfig,
}

impl Control {
    pub(crate) fn prepare_configuration(
        &mut self,
        replacement: Config,
        settings: CallSettings,
    ) -> Result<SettingsCandidate, ControlError> {
        if self.pending_configuration.is_some() {
            return Err(ControlError::Capacity);
        }
        replacement
            .validate_runtime()
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
        let endpoints = self.endpoints.prepare_configuration(&replacement)?;
        let dialplan = replacement.dialplan.clone();
        let websocket = replacement.websocket.clone();
        let key = self.settings.prepare(settings)?;
        self.pending_configuration = Some(PreparedConfiguration {
            key,
            config: replacement,
            endpoints,
            dialplan,
            websocket,
        });
        Ok(key)
    }

    pub(crate) fn cancel_configuration(
        &mut self,
        candidate: SettingsCandidate,
    ) -> Result<(), ControlError> {
        if !self
            .pending_configuration
            .as_ref()
            .is_some_and(|pending| pending.key == candidate)
        {
            return Err(ControlError::Conflict("Unknown configuration candidate"));
        }
        self.settings.cancel(candidate)?;
        self.pending_configuration = None;
        Ok(())
    }

    pub(crate) fn commit_configuration(
        &mut self,
        candidate: SettingsCandidate,
    ) -> Result<Config, ControlError> {
        let prepared = match self.pending_configuration.take() {
            Some(prepared) if prepared.key == candidate => prepared,
            Some(prepared) => {
                self.pending_configuration = Some(prepared);
                return Err(ControlError::Conflict("Unknown configuration candidate"));
            }
            None => return Err(ControlError::Conflict("Unknown configuration candidate")),
        };
        if let Err(error) = self.settings.commit(candidate) {
            self.pending_configuration = Some(prepared);
            return Err(error);
        }
        let PreparedConfiguration {
            config,
            mut endpoints,
            dialplan,
            websocket,
            key: _,
        } = prepared;
        endpoints.preserve_contacts(&self.endpoints);
        self.endpoints = endpoints;
        self.dialplan_program = dialplan;
        self.websocket = websocket;
        Ok(config)
    }

    pub(crate) fn configuration_generation(&self) -> rusterisk_core::SettingsGeneration {
        self.settings.current()
    }

    pub(crate) fn configuration_published(&mut self, timestamp: String) {
        self.info.configuration_published(timestamp);
    }
}
