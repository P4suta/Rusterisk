use super::{timestamp, Runner, MAX_PENDING_EFFECTS};
use crate::{Action, CallKey, ControlError, Outcome, Signal};
use std::net::SocketAddr;
use tokio::sync::oneshot;

pub(super) struct Pending {
    reply: oneshot::Sender<Result<Outcome, ControlError>>,
    deadline: u64,
}

impl Pending {
    pub(super) fn new(
        reply: oneshot::Sender<Result<Outcome, ControlError>>,
        deadline: u64,
    ) -> Self {
        Self { reply, deadline }
    }
}

impl Runner {
    pub(super) fn complete_external_admission(
        &mut self,
        key: CallKey,
        result: Result<SocketAddr, ControlError>,
        now: u64,
    ) -> Result<(), ControlError> {
        if self.external_admissions.get(&key).is_some_and(|pending| {
            pending.reply.is_closed() || now >= pending.deadline || !self.control.has_channel(&key)
        }) {
            self.expire_external_admission(&key, now)?;
        }
        let mut outcome = self.control.signal_at(
            Signal::ExternalRtpAdmitted {
                key: key.clone(),
                result: result.clone(),
            },
            &timestamp(),
            now,
        )?;
        self.publish(&mut outcome)?;
        if let Some(pending) = self.external_admissions.remove(&key) {
            let response = match result {
                Ok(_) if outcome.body.is_some() => Ok(outcome),
                Ok(_) => Err(ControlError::Conflict(
                    "Channel ended during external media admission",
                )),
                Err(error) => Err(error),
            };
            if pending.reply.send(response).is_err() {
                self.cancel_external_channel(&key, now)?;
            }
        }
        self.expire_external_admissions(now)
    }

    fn cancel_external_channel(&mut self, key: &CallKey, now: u64) -> Result<(), ControlError> {
        if self.control.has_channel(key) {
            let mut cleanup =
                self.control
                    .apply(Action::Hangup(key.id.clone()), &timestamp(), now)?;
            self.publish(&mut cleanup)?;
        }
        Ok(())
    }

    fn expire_external_admission(&mut self, key: &CallKey, now: u64) -> Result<(), ControlError> {
        let pending = self
            .external_admissions
            .remove(key)
            .ok_or(ControlError::Capacity)?;
        let error = if pending.reply.is_closed() || now >= pending.deadline {
            ControlError::Capacity
        } else {
            ControlError::Conflict("Channel ended during external media admission")
        };
        self.cancel_external_channel(key, now)?;
        if pending.reply.send(Err(error)).is_err() {
            tracing::debug!(channel = %key.id, generation = key.generation, "external media requester disconnected before admission");
        }
        Ok(())
    }

    pub(super) fn expire_external_admissions(&mut self, now: u64) -> Result<(), ControlError> {
        let expired = self
            .external_admissions
            .iter()
            .filter_map(|(key, pending)| {
                (pending.reply.is_closed()
                    || now >= pending.deadline
                    || !self.control.has_channel(key))
                .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        for key in expired {
            let pending_budget =
                MAX_PENDING_EFFECTS.saturating_sub(self.control.terminal_effect_reserve());
            let direct = self.pending_effects.is_empty() && self.effects.capacity() > 0;
            if !direct && self.pending_effects.len() >= pending_budget {
                break;
            }
            self.expire_external_admission(&key, now)?;
        }
        Ok(())
    }

    pub(super) fn close_external_admissions(&mut self) {
        for (key, pending) in std::mem::take(&mut self.external_admissions) {
            if pending
                .reply
                .send(Err(ControlError::Adapter("Control task stopped".into())))
                .is_err()
            {
                tracing::debug!(channel = %key.id, generation = key.generation, "external media requester disconnected during shutdown");
            }
        }
    }
}
