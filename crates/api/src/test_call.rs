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

pub(super) fn describe(paths: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::json;
    let mut parameters: Vec<_> = super::path_parameters(PATH)
        .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
        .collect();
    parameters.push(json!({"name":"deadline_seconds","in":"query","schema":{"type":"integer","minimum":1,"maximum":MAX_SECONDS,"default":DEFAULT_SECONDS}}));
    paths.insert(PATH.into(),json!({"servers":super::json_array([json!({"url":"/"})]),"post":{"operationId":"testCalls.probe","parameters":parameters,"security":super::json_array([json!({"basicAuth":super::json_array([])})]),"responses":{"200":{"description":"Test call milestones and verified audio result","content":{"application/json":{"schema":{"$ref":"#/components/schemas/TestCallResult"}}}}}}}));
}
