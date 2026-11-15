use crate::{api, Client, Error};

impl Client {
    pub async fn play_channel(
        &self,
        channel: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
    ) -> Result<api::Playback, Error> {
        self.play_media(
            "channels.playWithId",
            "channelId",
            channel,
            id,
            media,
            language,
        )
        .await
    }

    pub async fn play_bridge(
        &self,
        bridge: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
    ) -> Result<api::Playback, Error> {
        self.play_media(
            "bridges.playWithId",
            "bridgeId",
            bridge,
            id,
            media,
            language,
        )
        .await
    }

    async fn play_media(
        &self,
        operation: &str,
        resource: &str,
        target: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
    ) -> Result<api::Playback, Error> {
        let media = media.to_string();
        let mut query = Vec::with_capacity(2);
        query.push(("media", media.as_str()));
        if let Some(language) = language {
            query.push(("lang", language.as_str()));
        }
        self.request(
            operation,
            &[(resource, target.as_str()), ("playbackId", id.as_str())],
            &query,
        )
        .await
    }
}
