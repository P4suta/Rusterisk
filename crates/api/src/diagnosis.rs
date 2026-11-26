use crate::{ChannelId, ChannelState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelDiagnosis {
    pub id: ChannelId,
    pub generation: u64,
    pub creationtime: String,
    pub state: ChannelState,
    pub held: bool,
    pub hold_source: Option<HoldSource>,
    pub endpoint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldSource {
    Server,
    Peer,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    Present,
    Absent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Destination {
    pub endpoint: Option<String>,
    pub static_contact: Option<bool>,
    pub registration: Registration,
    pub available: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Missing,
    End,
    Dial,
    Stasis,
    Hangup,
    Dynamic,
    Loop,
    Limit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteStep {
    pub context: String,
    pub pattern: String,
    pub priority: u16,
    pub operation: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteDiagnosis {
    pub context: String,
    pub extension: String,
    pub steps: Vec<RouteStep>,
    pub disposition: Disposition,
    pub application: Option<String>,
    pub destinations: Vec<Destination>,
    pub channels: Vec<ChannelId>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct LoopActivity {
    pub turns: u64,
    pub late_turns: u64,
    pub drops: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeActivity {
    pub call_slots: CallSlots,
    pub control: LoopActivity,
    pub signaling: LoopActivity,
    pub media: LoopActivity,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct CallSlots {
    pub active: u64,
    pub awaiting_release: u64,
    pub limit: u64,
    pub capacity_refusals: u64,
}
