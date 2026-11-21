#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServerPacketSource {
    ServerRtp,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PeerReportSource {
    PeerRtcp,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ServerPacketCounts {
    pub source: ServerPacketSource,
    pub rxcount: u64,
    pub txcount: u64,
}

impl Default for ServerPacketCounts {
    fn default() -> Self {
        Self {
            source: ServerPacketSource::ServerRtp,
            rxcount: 0,
            txcount: 0,
        }
    }
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PeerReception {
    pub source: PeerReportSource,
    pub observed_at: String,
    pub reporter_ssrc: u32,
    pub reported_ssrc: u32,
    pub extended_highest_sequence: u32,
    pub cumulative_lost: i32,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RtpObservation {
    pub channel_id: crate::ChannelId,
    pub generation: u64,
    pub creationtime: String,
    pub observed_at: String,
    pub local: Option<ServerPacketCounts>,
    pub peer: Option<PeerReception>,
}
