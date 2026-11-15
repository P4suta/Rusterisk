use crate::{api, Client, Error};

impl Client {
    pub async fn stored_recordings(&self) -> Result<Vec<api::StoredRecording>, Error> {
        self.request("recordings.listStored", &[], &[]).await
    }

    pub async fn stored_recording(
        &self,
        name: &api::ChannelId,
    ) -> Result<api::StoredRecording, Error> {
        self.request(
            "recordings.getStored",
            &[("recordingName", name.as_str())],
            &[],
        )
        .await
    }

    pub async fn delete_stored_recording(&self, name: &api::ChannelId) -> Result<(), Error> {
        self.request(
            "recordings.deleteStored",
            &[("recordingName", name.as_str())],
            &[],
        )
        .await
    }
}
