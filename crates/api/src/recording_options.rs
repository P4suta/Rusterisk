use crate::RecordingIfExists;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct RecordingSeconds(#[schemars(with = "i32", range(min = 0, max = 2147483647))] u32);

impl RecordingSeconds {
    pub const fn seconds(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRecordingOption;

impl std::fmt::Display for InvalidRecordingOption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Invalid recording option")
    }
}
impl std::error::Error for InvalidRecordingOption {}

impl TryFrom<u32> for RecordingSeconds {
    type Error = InvalidRecordingOption;
    fn try_from(seconds: u32) -> Result<Self, Self::Error> {
        if seconds > i32::MAX as u32 {
            return Err(InvalidRecordingOption);
        }
        Ok(Self(seconds))
    }
}

impl<'de> Deserialize<'de> for RecordingSeconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RecordingTermination {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "*")]
    Star,
    #[serde(rename = "#")]
    Hash,
}

impl TryFrom<&str> for RecordingTermination {
    type Error = InvalidRecordingOption;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() || value.eq_ignore_ascii_case("none") {
            Ok(Self::None)
        } else if value.eq_ignore_ascii_case("any") {
            Ok(Self::Any)
        } else if value == "*" {
            Ok(Self::Star)
        } else if value == "#" {
            Ok(Self::Hash)
        } else {
            Err(InvalidRecordingOption)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingOptions {
    pub if_exists: RecordingIfExists,
    pub max_duration_seconds: RecordingSeconds,
    pub max_silence_seconds: RecordingSeconds,
    pub beep: bool,
    pub terminate_on: RecordingTermination,
}
