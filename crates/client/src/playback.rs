use crate::{api, Client, Error};

impl Client {
    pub async fn playback(&self, id: &api::ChannelId) -> Result<api::Playback, Error> {
        self.request("playbacks.get", &[("playbackId", id.as_str())], &[])
            .await
    }

    pub async fn stop_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("playbacks.stop", &[("playbackId", id.as_str())], &[])
            .await
    }

    pub async fn pause_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.playback_control(id, "pause").await
    }

    pub async fn resume_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.playback_control(id, "unpause").await
    }

    pub async fn restart_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.playback_control(id, "restart").await
    }

    pub async fn forward_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.playback_control(id, "forward").await
    }

    pub async fn reverse_playback(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.playback_control(id, "reverse").await
    }

    async fn playback_control(&self, id: &api::ChannelId, operation: &str) -> Result<(), Error> {
        self.request(
            "playbacks.control",
            &[("playbackId", id.as_str())],
            &[("operation", operation)],
        )
        .await
    }

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
