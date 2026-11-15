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
        let mut canonical = [0u8; 16];
        if value.len() > canonical.len() || !value.is_ascii() {
            return Err(InvalidRecordingIfExists);
        }
        canonical[..value.len()].copy_from_slice(value.as_bytes());
        canonical.make_ascii_lowercase();
        let canonical =
            std::str::from_utf8(&canonical[..value.len()]).map_err(|_| InvalidRecordingIfExists)?;
        Self::deserialize(
            serde::de::value::StrDeserializer::<serde::de::value::Error>::new(canonical),
        )
        .map_err(|_| InvalidRecordingIfExists)
    }
}
