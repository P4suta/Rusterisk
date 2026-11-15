use super::{receive, timestamp, Controller, Request, Runner, MAX_PENDING_EFFECTS};
use crate::{media_websocket::MediaLinkOptions, CallKey, ControlError, Signal};
use rusterisk_api::ChannelId;
use tokio::sync::{mpsc, oneshot};

pub struct MediaConnectionLease {
    _lifetime: mpsc::Sender<()>,
}

pub struct ClaimedMediaConnection {
    pub key: CallKey,
    pub options: MediaLinkOptions,
    pub lease: MediaConnectionLease,
}

impl Controller {
    pub async fn claim_media_connection(
        &self,
        connection_id: ChannelId,
        origin: Option<String>,
    ) -> Result<ClaimedMediaConnection, ControlError> {
        let (sender, lifetime) = mpsc::channel(1);
        let (reply, response) = oneshot::channel();
        self.requests
            .try_send(Request::ClaimMediaConnection {
                connection_id,
                origin,
                lifetime,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        let (key, options) = receive(response).await?;
        Ok(ClaimedMediaConnection {
            key,
            options,
            lease: MediaConnectionLease { _lifetime: sender },
        })
    }
}

impl Runner {
    pub(super) fn claim_media_request(
        &mut self,
        connection_id: ChannelId,
        origin: Option<String>,
        lifetime: mpsc::Receiver<()>,
        reply: oneshot::Sender<Result<(CallKey, MediaLinkOptions), ControlError>>,
    ) {
        if reply.is_closed() || lifetime.is_closed() {
            return;
        }
        let result = if origin
            .as_ref()
            .is_some_and(|origin| !self.config.origins.contains(origin))
        {
            Err(ControlError::Forbidden)
        } else if self.media_leases.len() >= 64 || !self.pending_effects.is_empty() {
            Err(ControlError::Capacity)
        } else {
            self.control
                .claim_media_connection(&connection_id)
                .map(|(key, options)| {
                    self.media_leases.insert(key.clone(), lifetime);
                    (key, options)
                })
        };
        if reply.send(result).is_err() {
            tracing::debug!("media connection requester disconnected");
        }
    }

    pub(super) fn expire_media_connections(&mut self, now: u64) -> Result<(), ControlError> {
        self.media_leases
            .retain(|key, _| self.control.has_channel(key));
        let released: Vec<CallKey> = self
            .media_leases
            .iter()
            .filter_map(|(key, lifetime)| lifetime.is_closed().then_some(key.clone()))
            .collect();
        for key in released {
            let pending_budget =
                MAX_PENDING_EFFECTS.saturating_sub(self.control.terminal_effect_reserve());
            let direct = self.pending_effects.is_empty() && self.effects.capacity() > 0;
            if !direct && self.pending_effects.len() >= pending_budget {
                break;
            }
            self.media_leases.remove(&key);
            let mut outcome =
                self.control
                    .signal_at(Signal::Terminated { key, cause: 16 }, &timestamp(), now)?;
            self.publish(&mut outcome)?;
        }
        Ok(())
    }
}
