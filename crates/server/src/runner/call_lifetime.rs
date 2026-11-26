use super::{Controller, Request, Runner};
use crate::runner::calls;
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
            .map_err(ControlError::queue)?;
        self.receive(response).await?;
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
        self.records.retain(calls::LIFETIME, |key, lifetime| {
            !lifetime.is_closed() && self.control.has_channel(key)
        });
        let result = if !self.control.has_channel(&key) {
            Err(ControlError::Conflict(
                "Channel generation is no longer active",
            ))
        } else if self.records.get(calls::LIFETIME, &key).is_some() {
            Err(ControlError::Conflict("Call lifetime already claimed"))
        } else if self.records.iter(calls::LIFETIME).count() >= 64 {
            Err(ControlError::Capacity)
        } else {
            self.records.insert(calls::LIFETIME, key.clone(), lifetime)
        };
        let inserted = result.is_ok();
        if reply.send(result).is_err() {
            if inserted {
                self.records.remove(calls::LIFETIME, &key);
            }
            tracing::debug!("Call lifetime requester disconnected");
        }
    }
}
