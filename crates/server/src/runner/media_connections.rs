use super::{timestamp, ClosureNotice, Controller, Request, Runner, MAX_PENDING_EFFECTS};
use crate::runner::calls;
use crate::{media_websocket::MediaLinkOptions, CallKey, ControlError, Signal};
use rusterisk_api::ChannelId;
use tokio::sync::{mpsc, oneshot};

pub struct MediaConnectionLease {
    lifetime: mpsc::Sender<()>,
    _closure: ClosureNotice,
}

impl MediaConnectionLease {
    pub fn is_released(&self) -> bool {
        self.lifetime.is_closed()
    }
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
        let closure = self.closures.notice();
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
        let (key, options) = self.receive(response).await?;
        Ok(ClaimedMediaConnection {
            key,
            options,
            lease: MediaConnectionLease {
                lifetime: sender,
                _closure: closure,
            },
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
        } else if self.records.iter(calls::CONNECTION).count() >= 64 || !self.effects_flowing() {
            Err(ControlError::Capacity)
        } else {
            self.control
                .claim_media_connection(&connection_id)
                .and_then(|(key, options)| {
                    self.records
                        .insert(calls::CONNECTION, key.clone(), lifetime)?;
                    Ok((key, options))
                })
        };
        if reply.send(result).is_err() {
            tracing::debug!("media connection requester disconnected");
        }
    }

    pub(super) fn expire_media_connections(&mut self, now: u64) -> Result<(), ControlError> {
        self.records
            .retain(calls::CONNECTION, |key, _| self.control.has_channel(key));
        let released: Vec<CallKey> = self
            .records
            .iter(calls::CONNECTION)
            .filter_map(|(key, lifetime)| lifetime.is_closed().then_some(key.clone()))
            .collect();
        for key in released {
            let pending_budget =
                MAX_PENDING_EFFECTS.saturating_sub(self.control.terminal_effect_reserve());
            let direct = self.effect_room() > 0;
            if !direct && self.pending_effects.len() >= pending_budget {
                break;
            }
            self.records.remove(calls::CONNECTION, &key);
            let mut outcome =
                self.control
                    .signal_at(Signal::Terminated { key, cause: 16 }, &timestamp(), now)?;
            self.publish(&mut outcome)?;
        }
        Ok(())
    }
}
