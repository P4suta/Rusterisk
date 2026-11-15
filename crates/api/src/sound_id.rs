use crate::ChannelId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct SoundId(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSoundId;

impl std::fmt::Display for InvalidSoundId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("sound name requires at most 128 ASCII bytes and eight relative components")
    }
}

impl std::error::Error for InvalidSoundId {}

impl TryFrom<String> for SoundId {
    type Error = InvalidSoundId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 128 || value.split('/').count() > 8 {
            return Err(InvalidSoundId);
        }
        for component in value.split('/') {
            if component.is_empty()
                || matches!(component, "." | "..")
                || !component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                return Err(InvalidSoundId);
            }
        }
        Ok(Self(value))
    }
}

impl From<ChannelId> for SoundId {
    fn from(value: ChannelId) -> Self {
        Self(value.into())
    }
}

impl From<SoundId> for String {
    fn from(value: SoundId) -> Self {
        value.0
    }
}

impl SoundId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SoundId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
