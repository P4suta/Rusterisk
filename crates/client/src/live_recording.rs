use crate::{api, Client, Error};

#[derive(serde::Serialize)]
struct Creation<'a> {
    name: &'a api::ChannelId,
    format: &'a str,
    #[serde(flatten)]
    options: api::RecordingOptions,
}

impl Client {
    pub async fn record_channel(
        &self,
        channel: &api::ChannelId,
        name: &api::ChannelId,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<api::LiveRecording, Error> {
        self.start_recording(
            "channels.record",
            "channelId",
            channel,
            name,
            format,
            options,
        )
        .await
    }

    pub async fn record_bridge(
        &self,
        bridge: &api::ChannelId,
        name: &api::ChannelId,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<api::LiveRecording, Error> {
        self.start_recording("bridges.record", "bridgeId", bridge, name, format, options)
            .await
    }

    async fn start_recording(
        &self,
        operation: &str,
        parameter: &str,
        target: &api::ChannelId,
        name: &api::ChannelId,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<api::LiveRecording, Error> {
        if format.is_empty() || format.len() > 32 || format.chars().any(char::is_control) {
            return Err(Error::Invalid("invalid recording format"));
        }
        self.request_body(
            operation,
            &[(parameter, target.as_str())],
            &[],
            &Creation {
                name,
                format,
                options,
            },
        )
        .await
    }
}
