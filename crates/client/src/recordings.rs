use crate::{api, Client, Error};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};

const MAX_RECORDING_BYTES: u64 = 1024 * 1024 * 1024 + 44;

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

    pub async fn download_stored_recording<W: AsyncWrite + Unpin>(
        &self,
        name: &api::ChannelId,
        output: &mut W,
    ) -> Result<u64, Error> {
        let request = self
            .prepare_request(
                "recordings.getStoredFile",
                &[("recordingName", name.as_str())],
                &[],
            )?
            .timeout(Duration::from_secs(600));
        let mut response = tokio::time::timeout(Duration::from_secs(10), request.send())
            .await
            .map_err(|_| Error::Timeout)??;
        if !response.status().is_success() {
            return tokio::time::timeout(Duration::from_secs(10), Self::decode_response(response))
                .await
                .map_err(|_| Error::Timeout)?;
        }
        let length = response.content_length();
        if length.is_some_and(|length| length > MAX_RECORDING_BYTES) {
            return Err(Error::Capacity);
        }
        let mut written = 0u64;
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(5), response.chunk())
                .await
                .map_err(|_| Error::Timeout)??;
            let Some(chunk) = chunk else {
                break;
            };
            let next = written
                .checked_add(chunk.len() as u64)
                .ok_or(Error::Capacity)?;
            if next > MAX_RECORDING_BYTES {
                return Err(Error::Capacity);
            }
            tokio::time::timeout(Duration::from_secs(5), output.write_all(&chunk))
                .await
                .map_err(|_| Error::Timeout)??;
            written = next;
        }
        if length.is_some_and(|length| length != written) {
            return Err(Error::Invalid("recording length mismatch"));
        }
        Ok(written)
    }
}
