use crate::{api, Client, DtmfDigit, DtmfTiming, Error, MuteDirection};

impl Client {
    pub async fn ring(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.ring", &[("channelId", id.as_str())], &[])
            .await
    }

    pub async fn stop_ringing(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.ringStop", &[("channelId", id.as_str())], &[])
            .await
    }

    pub async fn hold(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.hold", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn unhold(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.unhold", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn start_music_on_hold(
        &self,
        id: &api::ChannelId,
        class: Option<&str>,
    ) -> Result<(), Error> {
        let path = [("channelId", id.as_str())];
        match class {
            Some(class) => {
                self.request("channels.startMoh", &path, &[("mohClass", class)])
                    .await
            }
            None => self.request("channels.startMoh", &path, &[]).await,
        }
    }
    pub async fn stop_music_on_hold(&self, id: &api::ChannelId) -> Result<(), Error> {
        self.request("channels.stopMoh", &[("channelId", id.as_str())], &[])
            .await
    }
    pub async fn mute(&self, id: &api::ChannelId, direction: MuteDirection) -> Result<(), Error> {
        self.audio_direction("channels.mute", id, direction).await
    }
    pub async fn unmute(&self, id: &api::ChannelId, direction: MuteDirection) -> Result<(), Error> {
        self.audio_direction("channels.unmute", id, direction).await
    }
    async fn audio_direction(
        &self,
        operation: &str,
        id: &api::ChannelId,
        direction: MuteDirection,
    ) -> Result<(), Error> {
        let direction = match direction {
            MuteDirection::Incoming => "in",
            MuteDirection::Outgoing => "out",
            MuteDirection::Both => "both",
        };
        self.request(
            operation,
            &[("channelId", id.as_str())],
            &[("direction", direction)],
        )
        .await
    }
    pub async fn variable(&self, id: &api::ChannelId, name: &str) -> Result<api::Variable, Error> {
        self.request(
            "channels.getChannelVar",
            &[("channelId", id.as_str())],
            &[("variable", name)],
        )
        .await
    }
    pub async fn set_variable(
        &self,
        id: &api::ChannelId,
        name: &str,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let mut query: Vec<(&str, &str)> = Vec::with_capacity(2);
        query.push(("variable", name));
        if let Some(value) = value {
            query.push(("value", value));
        }
        self.request(
            "channels.setChannelVar",
            &[("channelId", id.as_str())],
            &query,
        )
        .await
    }
    pub async fn send_dtmf(
        &self,
        id: &api::ChannelId,
        digits: &[DtmfDigit],
        timing: DtmfTiming,
    ) -> Result<(), Error> {
        if digits.is_empty() || digits.len() > 32 {
            return Err(Error::Invalid("use 1 through 32 telephone digits"));
        }
        let digits: String = digits.iter().map(|digit| digit.symbol()).collect();
        let before = timing.before_ms().to_string();
        let between = timing.between_ms().to_string();
        let duration = timing.duration_ms().to_string();
        let after = timing.after_ms().to_string();
        self.request(
            "channels.sendDTMF",
            &[("channelId", id.as_str())],
            &[
                ("dtmf", &digits),
                ("before", &before),
                ("between", &between),
                ("duration", &duration),
                ("after", &after),
            ],
        )
        .await
    }
}
