use super::{receive, Controller, Credentials, Request, Runner};
use crate::ControlError;
use rusterisk_api::ChannelId;
use serde_json::{to_value, Value};
use tokio::sync::oneshot;

pub(crate) enum Target {
    Channel(ChannelId),
    Extension { context: String, extension: String },
}

impl Controller {
    pub(crate) async fn diagnose(
        &self,
        credentials: Credentials,
        origin: Option<String>,
        target: Target,
    ) -> Result<Value, ControlError> {
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::Diagnose {
                credentials,
                origin,
                target,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        receive(response).await
    }
}

impl Runner {
    pub(super) fn diagnose(&self, target: Target) -> Result<Value, ControlError> {
        Ok(match target {
            Target::Channel(id) => to_value(self.control.diagnose_channel(&id)?)?,
            Target::Extension { context, extension } => {
                to_value(self.control.diagnose_extension(context, extension))?
            }
        })
    }
}
