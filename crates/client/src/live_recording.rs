use crate::{api, Client, Error};

#[derive(serde::Serialize)]
struct Creation<'a> {
    name: &'a crate::RecordingName,
    format: &'a str,
    #[serde(flatten)]
    options: api::RecordingOptions,
}

impl Client {
    pub async fn live_recording(
        &self,
        name: &crate::RecordingName,
    ) -> Result<crate::LiveRecording, Error> {
        self.request(
            "recordings.getLive",
            &[("recordingName", name.as_str())],
            &[],
        )
        .await
    }

    pub async fn cancel_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.cancel", name).await
    }

    pub async fn stop_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.stop", name).await
    }

    pub async fn pause_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.pause", name).await
    }

    pub async fn resume_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.unpause", name).await
    }

    pub async fn mute_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.mute", name).await
    }

    pub async fn unmute_recording(&self, name: &crate::RecordingName) -> Result<(), Error> {
        self.recording_control("recordings.unmute", name).await
    }

    async fn recording_control(
        &self,
        operation: &str,
        name: &crate::RecordingName,
    ) -> Result<(), Error> {
        self.request(operation, &[("recordingName", name.as_str())], &[])
            .await
    }

    pub async fn record_channel(
        &self,
        channel: &api::ChannelId,
        name: &crate::RecordingName,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<crate::LiveRecording, Error> {
        self.start_recording(
            "channels.record",
            "channelId",
            channel.as_str(),
            name,
            format,
            options,
        )
        .await
    }

    pub async fn record_bridge(
        &self,
        bridge: &crate::BridgeId,
        name: &crate::RecordingName,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<crate::LiveRecording, Error> {
        self.start_recording(
            "bridges.record",
            "bridgeId",
            bridge.as_str(),
            name,
            format,
            options,
        )
        .await
    }

    async fn start_recording(
        &self,
        operation: &str,
        parameter: &str,
        target: &str,
        name: &crate::RecordingName,
        format: &str,
        options: api::RecordingOptions,
    ) -> Result<crate::LiveRecording, Error> {
        if format.is_empty() || format.len() > 32 || format.chars().any(char::is_control) {
            return Err(Error::Invalid("invalid recording format"));
        }
        self.request_body(
            operation,
            &[(parameter, target)],
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
