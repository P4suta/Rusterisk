use crate::Engine;
use rusterisk_config::{Config, ConfigError};

pub struct PreparedEngineConfiguration {
    config: Config,
}

impl PreparedEngineConfiguration {
    /// Validates typed runtime values without reading external resources.
    pub fn prepare(config: Config) -> Result<Self, ConfigError> {
        config.validate_runtime()?;
        Ok(Self { config })
    }
}

impl Engine {
    /// Applies settings to future admissions while retaining existing signaling state.
    pub fn publish_configuration(&mut self, prepared: PreparedEngineConfiguration) {
        self.config = prepared.config;
    }
}
