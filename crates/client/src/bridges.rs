use crate::{api, Client, Error};

impl Client {
    pub async fn add_channels(
        &self,
        bridge: &crate::BridgeId,
        channels: &[api::ChannelId],
    ) -> Result<(), Error> {
        self.bridge_membership("bridges.addChannel", bridge, channels)
            .await
    }

    pub async fn remove_channels(
        &self,
        bridge: &crate::BridgeId,
        channels: &[api::ChannelId],
    ) -> Result<(), Error> {
        self.bridge_membership("bridges.removeChannel", bridge, channels)
            .await
    }

    async fn bridge_membership(
        &self,
        operation: &str,
        bridge: &crate::BridgeId,
        channels: &[api::ChannelId],
    ) -> Result<(), Error> {
        if channels.is_empty() {
            return Err(Error::Invalid("bridge channels are required"));
        }
        if channels.len() > 32 {
            return Err(Error::Capacity);
        }
        let channels = channels
            .iter()
            .map(api::ChannelId::as_str)
            .collect::<Vec<_>>()
            .join(",");
        self.request(
            operation,
            &[("bridgeId", bridge.as_str())],
            &[("channel", &channels)],
        )
        .await
    }
}
