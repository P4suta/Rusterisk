use crate::Engine;
use rusterisk_config::{Config, ConfigError};

/// Controls new inbound calls and registration transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionMode {
    Open,
    Quiesced,
}

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
    pub fn set_admission(&mut self, mode: AdmissionMode) {
        self.admission = mode;
    }

    /// Applies settings to future admissions while retaining existing signaling state.
    pub fn publish_configuration(&mut self, prepared: PreparedEngineConfiguration) {
        self.config = prepared.config;
    }
}
