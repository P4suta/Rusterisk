use crate::{ChannelId, ChannelState, EndpointState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CHANNEL_PATH: &str = "/rusterisk/observe/channels/{channelId}";
pub const ENDPOINT_PATH: &str = "/rusterisk/observe/endpoints/{technology}/{resource}";
pub const CONTENT_TYPE: &str = "application/x-ndjson";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedLeg {
    pub id: ChannelId,
    pub generation: u64,
    pub state: ChannelState,
    pub endpoint: String,
    pub bridge_ids: Vec<ChannelId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedEndpoint {
    pub technology: String,
    pub resource: String,
    pub state: Option<EndpointState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchRecord {
    pub timestamp: String,
    #[serde(flatten)]
    pub entry: WatchEntry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchEntry {
    Snapshot {
        channels: Vec<ObservedLeg>,
        endpoint: Option<ObservedEndpoint>,
    },
    Event {
        event: String,
        channel: Option<ObservedLeg>,
        endpoint: Option<ObservedEndpoint>,
        bridge_id: Option<ChannelId>,
    },
    Gap {
        reason: String,
        skipped: Option<u64>,
    },
}

pub(super) fn describe(paths: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::json;
    for (path, id) in [
        (CHANNEL_PATH, "observe.channel"),
        (ENDPOINT_PATH, "observe.endpoint"),
    ] {
        let parameters: Vec<_> = super::path_parameters(path)
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        paths.insert(path.into(),json!({"servers":super::json_array([json!({"url":"/"})]),"get":{"operationId":id,"parameters":parameters,"security":super::json_array([json!({"basicAuth":super::json_array([])})]),"responses":{"200":{"description":"Stream of timestamped call observations","content":{(CONTENT_TYPE):{"schema":{"type":"string"},"x-ndjson-item":{"$ref":"#/components/schemas/WatchRecord"}}}}}}}));
    }
}
