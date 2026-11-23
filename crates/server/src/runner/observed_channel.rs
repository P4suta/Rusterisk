use super::{Controller, Credentials, Request};
use crate::{CallKey, ControlError};
use rusterisk_api::ChannelId;
use tokio::sync::oneshot;

impl Controller {
    pub(crate) async fn observed_channel(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        channel: ChannelId,
    ) -> Result<(CallKey, String), ControlError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::ObservedChannel {
                credentials,
                origin,
                channel,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        self.receive(response).await
    }
}
