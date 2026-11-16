use crate::{api, Client, Error};

enum Target<'a> {
    Channel(&'a api::ChannelId),
    Bridge(&'a api::ChannelId),
}

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
        self.play_media(Target::Channel(channel), id, media, language, None)
            .await
    }

    pub async fn play_bridge(
        &self,
        bridge: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
    ) -> Result<api::Playback, Error> {
        self.play_media(Target::Bridge(bridge), id, media, language, None)
            .await
    }

    pub async fn play_channel_with_options(
        &self,
        channel: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
        options: api::PlaybackOptions,
    ) -> Result<api::Playback, Error> {
        self.play_media(Target::Channel(channel), id, media, language, Some(options))
            .await
    }

    pub async fn play_bridge_with_options(
        &self,
        bridge: &api::ChannelId,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
        options: api::PlaybackOptions,
    ) -> Result<api::Playback, Error> {
        self.play_media(Target::Bridge(bridge), id, media, language, Some(options))
            .await
    }

    async fn play_media(
        &self,
        target: Target<'_>,
        id: &api::ChannelId,
        media: &api::MediaUri,
        language: Option<&api::ChannelId>,
        options: Option<api::PlaybackOptions>,
    ) -> Result<api::Playback, Error> {
        let (operation, resource, target) = match target {
            Target::Channel(id) => ("channels.playWithId", "channelId", id),
            Target::Bridge(id) => ("bridges.playWithId", "bridgeId", id),
        };
        let media = media.to_string();
        let offset = options.map(|options| options.offset_ms.milliseconds().to_string());
        let step = options.map(|options| options.skip_ms.milliseconds().to_string());
        let mut query = Vec::with_capacity(4);
        query.push(("media", media.as_str()));
        if let Some(language) = language {
            query.push(("lang", language.as_str()));
        }
        if let Some(offset) = &offset {
            query.push(("offsetms", offset.as_str()));
        }
        if let Some(step) = &step {
            query.push(("skipms", step.as_str()));
        }
        self.request(
            operation,
            &[(resource, target.as_str()), ("playbackId", id.as_str())],
            &query,
        )
        .await
    }
}
