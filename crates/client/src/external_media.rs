use crate::{api, Client, Error};

impl Client {
    pub async fn external_websocket(
        &self,
        id: &api::ChannelId,
        app: &str,
        options: api::WebSocketMediaOptions,
        data: &str,
    ) -> Result<api::Channel, Error> {
        api::ChannelId::try_from(app.to_owned())?;
        let transport_data = options.transport_data();
        self.request(
            "channels.externalMedia",
            &[],
            &[
                ("channelId", id.as_str()),
                ("app", app),
                ("format", options.format.as_str()),
                ("encapsulation", "none"),
                ("transport", "websocket"),
                ("connection_type", "server"),
                ("direction", options.direction.as_str()),
                ("data", data),
                ("transport_data", &transport_data),
            ],
        )
        .await
    }

    pub async fn external_audio_socket(
        &self,
        id: &api::ChannelId,
        app: &str,
        target: api::AudioSocketTarget,
    ) -> Result<api::Channel, Error> {
        api::ChannelId::try_from(app.to_owned())?;
        let remote = target.remote().to_string();
        let session = uuid::Uuid::from_bytes(target.session())
            .hyphenated()
            .to_string();
        self.request(
            "channels.externalMedia",
            &[],
            &[
                ("channelId", id.as_str()),
                ("app", app),
                ("external_host", &remote),
                ("format", "slin"),
                ("encapsulation", "audiosocket"),
                ("transport", "tcp"),
                ("connection_type", "client"),
                ("direction", "both"),
                ("data", &session),
            ],
        )
        .await
    }

    pub async fn external_rtp(
        &self,
        id: &api::ChannelId,
        app: &str,
        target: api::ExternalRtpTarget,
        data: &str,
    ) -> Result<api::Channel, Error> {
        api::ChannelId::try_from(app.to_owned())?;
        let remote = target.remote().to_string();
        self.request(
            "channels.externalMedia",
            &[],
            &[
                ("channelId", id.as_str()),
                ("app", app),
                ("external_host", &remote),
                ("format", target.codec().as_str()),
                ("encapsulation", "rtp"),
                ("transport", "udp"),
                ("connection_type", "client"),
                ("direction", "both"),
                ("data", data),
            ],
        )
        .await
    }
}
