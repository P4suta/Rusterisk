use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct PlaybackMilliseconds(#[schemars(with = "i32", range(min = 0, max = 2147483647))] u32);

impl PlaybackMilliseconds {
    pub const fn milliseconds(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidPlaybackOption;

impl std::fmt::Display for InvalidPlaybackOption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Invalid playback option")
    }
}

impl std::error::Error for InvalidPlaybackOption {}

impl TryFrom<u32> for PlaybackMilliseconds {
    type Error = InvalidPlaybackOption;

    fn try_from(milliseconds: u32) -> Result<Self, Self::Error> {
        if milliseconds > i32::MAX as u32 {
            return Err(InvalidPlaybackOption);
        }
        Ok(Self(milliseconds))
    }
}

impl<'de> Deserialize<'de> for PlaybackMilliseconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackOptions {
    #[serde(rename = "offsetms")]
    pub offset_ms: PlaybackMilliseconds,
    #[serde(rename = "skipms")]
    pub skip_ms: PlaybackMilliseconds,
}

impl PlaybackOptions {
    pub const fn step_ms(self) -> u32 {
        match self.skip_ms.milliseconds() {
            0 => 3000,
            milliseconds => milliseconds,
        }
    }
}
