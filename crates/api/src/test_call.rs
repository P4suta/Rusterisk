use crate::ChannelId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PATH: &str = "/rusterisk/test-calls/{technology}/{resource}";
pub const MAX_SECONDS: u16 = 120;
pub const DEFAULT_SECONDS: u16 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestCallOutcome {
    Verified,
    Busy,
    NoAnswer,
    Silent,
    SignalMismatch,
    CallEnded,
    MediaUnavailable,
    NotAccepted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestCallResult {
    pub channel_id: ChannelId,
    pub accepted: bool,
    pub answered: bool,
    pub signal_verified: bool,
    pub outcome: TestCallOutcome,
}
