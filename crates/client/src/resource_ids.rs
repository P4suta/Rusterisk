//! ARI uses separate identifiers for channels, bridges, playbacks, and recording names.
//!
//! ```compile_fail
//! use rusterisk_client::{api::ChannelId, Client};
//! async fn destroy(client: &Client, channel: &ChannelId) {
//!     let _result = client.destroy_bridge(channel).await;
//! }
//! ```
//!
//! ```compile_fail
//! use rusterisk_client::{api::ChannelId, Client};
//! async fn stop(client: &Client, channel: &ChannelId) {
//!     let _result = client.stop_playback(channel).await;
//! }
//! ```
//!
//! ```compile_fail
//! use rusterisk_client::{api::ChannelId, Client};
//! async fn stop(client: &Client, channel: &ChannelId) {
//!     let _result = client.stop_recording(channel).await;
//! }
//! ```

pub use crate::api::ChannelId;

macro_rules! resource_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(ChannelId);

        impl $name {
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl TryFrom<String> for $name {
            type Error = crate::api::InvalidId;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                ChannelId::try_from(value).map(Self)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

resource_id!(BridgeId);
resource_id!(PlaybackId);
resource_id!(RecordingName);

impl From<BridgeId> for crate::api::EventSource {
    fn from(id: BridgeId) -> Self {
        Self::Bridge(id)
    }
}
