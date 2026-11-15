use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RecordingIfExists {
    #[default]
    Fail,
    Overwrite,
    Append,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRecordingIfExists;

impl std::fmt::Display for InvalidRecordingIfExists {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Invalid recording conflict policy")
    }
}

impl std::error::Error for InvalidRecordingIfExists {}

impl TryFrom<&str> for RecordingIfExists {
    type Error = InvalidRecordingIfExists;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Ok(Self::default());
        }
        Self::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(value))
            .map_err(|_| InvalidRecordingIfExists)
    }
}
