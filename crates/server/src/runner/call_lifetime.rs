use super::{receive, Controller, Request, Runner};
use crate::{CallKey, ControlError};
use tokio::sync::{mpsc, oneshot};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct CallLease {
    lifetime: mpsc::Sender<()>,
}

impl CallLease {
    pub(crate) fn is_released(&self) -> bool {
        self.lifetime.is_closed()
    }

    pub(crate) async fn released(&self) {
        self.lifetime.closed().await;
    }
}

impl Controller {
    pub(crate) async fn call_lifetime(&self, key: CallKey) -> Result<CallLease, ControlError> {
        let (sender, lifetime) = mpsc::channel(1);
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::CallLifetime {
                key,
                lifetime,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        receive(response).await?;
        Ok(CallLease { lifetime: sender })
    }
}

impl Runner {
    pub(super) fn claim_call_lifetime(
        &mut self,
        key: CallKey,
        lifetime: mpsc::Receiver<()>,
        reply: oneshot::Sender<Result<(), ControlError>>,
    ) {
        if reply.is_closed() || lifetime.is_closed() {
            return;
        }
        self.call_lifetimes
            .retain(|key, lifetime| !lifetime.is_closed() && self.control.has_channel(key));
        let result = if !self.control.has_channel(&key) {
            Err(ControlError::Conflict(
                "Channel generation is no longer active",
            ))
        } else if self.call_lifetimes.contains_key(&key) {
            Err(ControlError::Conflict("Call lifetime already claimed"))
        } else if self.call_lifetimes.len() >= 64 {
            Err(ControlError::Capacity)
        } else {
            self.call_lifetimes.insert(key.clone(), lifetime);
            Ok(())
        };
        let inserted = result.is_ok();
        if reply.send(result).is_err() {
            if inserted {
                self.call_lifetimes.remove(&key);
            }
            tracing::debug!("Call lifetime requester disconnected");
        }
    }
}
