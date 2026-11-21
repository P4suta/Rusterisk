use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedEvent {
    #[serde(rename = "type")]
    name: Name,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

impl PublishedEvent {
    pub const fn event_type(&self) -> &'static str {
        match self.name {
            Name::ApplicationRegistered => "ApplicationRegistered",
            Name::ApplicationUnregistered => "ApplicationUnregistered",
            Name::BridgeAttendedTransfer => "BridgeAttendedTransfer",
            Name::BridgeBlindTransfer => "BridgeBlindTransfer",
            Name::BridgeMerged => "BridgeMerged",
            Name::BridgeVideoSourceChanged => "BridgeVideoSourceChanged",
            Name::ChannelConnectedLine => "ChannelConnectedLine",
            Name::ChannelTalkingFinished => "ChannelTalkingFinished",
            Name::ChannelTalkingStarted => "ChannelTalkingStarted",
            Name::ChannelToneDetected => "ChannelToneDetected",
            Name::ChannelTransfer => "ChannelTransfer",
            Name::ContactStatusChange => "ContactStatusChange",
            Name::DeviceStateChanged => "DeviceStateChanged",
            Name::PeerStatusChange => "PeerStatusChange",
            Name::RestResponse => "RESTResponse",
            Name::TextMessageReceived => "TextMessageReceived",
        }
    }

    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Name {
    ApplicationRegistered,
    ApplicationUnregistered,
    BridgeAttendedTransfer,
    BridgeBlindTransfer,
    BridgeMerged,
    BridgeVideoSourceChanged,
    ChannelConnectedLine,
    ChannelTalkingFinished,
    ChannelTalkingStarted,
    ChannelToneDetected,
    ChannelTransfer,
    ContactStatusChange,
    DeviceStateChanged,
    PeerStatusChange,
    #[serde(rename = "RESTResponse")]
    RestResponse,
    TextMessageReceived,
}
