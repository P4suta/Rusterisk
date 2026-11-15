use super::{timestamp, Runner, MAX_PENDING_EFFECTS};
use crate::{ControlError, MediaOwner, Outcome, Signal};
use tokio::sync::oneshot;

pub(super) struct Pending {
    owner: MediaOwner,
    reply: oneshot::Sender<Result<Outcome, ControlError>>,
    outcome: Outcome,
    deadline: u64,
}

impl Pending {
    pub(super) fn new(
        owner: MediaOwner,
        reply: oneshot::Sender<Result<Outcome, ControlError>>,
        outcome: Outcome,
        deadline: u64,
    ) -> Self {
        Self {
            owner,
            reply,
            outcome,
            deadline,
        }
    }
}

impl Runner {
    pub(super) fn apply_signal(&mut self, signal: Signal, now: u64) -> Result<(), ControlError> {
        self.expire_recording_admissions(now)?;
        let completion = match &signal {
            Signal::RecordingAdmitted {
                owner,
                token,
                result,
            } => Some((owner.clone(), *token, result.clone())),
            Signal::RecordingFinished {
                owner,
                token,
                result,
            } => Some((
                owner.clone(),
                *token,
                Err(result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or(ControlError::Adapter(
                        "Recording ended before admission".into(),
                    ))),
            )),
            Signal::MediaPlaybackStarted { .. }
            | Signal::MediaPlaybackFinished { .. }
            | Signal::PlaybackStarted { .. }
            | Signal::PlaybackFinished { .. }
            | Signal::DtmfReady { .. }
            | Signal::DtmfFinished { .. }
            | Signal::DtmfReceived { .. }
            | Signal::Milestone { .. }
            | Signal::Terminated { .. } => None,
        };
        let mut outcome = self.control.signal_at(signal, &timestamp(), now)?;
        self.publish(&mut outcome)?;
        if let Some((owner, token, result)) = completion {
            if self
                .recording_admissions
                .get(&token)
                .is_some_and(|pending| pending.owner == owner)
            {
                let pending = self
                    .recording_admissions
                    .remove(&token)
                    .ok_or(ControlError::Capacity)?;
                let reply = result.map(|()| pending.outcome);
                if pending.reply.send(reply).is_err() {
                    let mut outcome = self.control.cancel_recording_admission(&owner, token)?;
                    self.publish(&mut outcome)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn expire_recording_admissions(&mut self, now: u64) -> Result<(), ControlError> {
        let expired = self
            .recording_admissions
            .iter()
            .filter_map(|(token, pending)| {
                (pending.reply.is_closed() || now >= pending.deadline).then_some(*token)
            })
            .collect::<Vec<_>>();
        for token in expired {
            if self.pending_effects.len() >= MAX_PENDING_EFFECTS {
                break;
            }
            let pending = self
                .recording_admissions
                .remove(&token)
                .ok_or(ControlError::Capacity)?;
            let mut outcome = self
                .control
                .cancel_recording_admission(&pending.owner, token)?;
            self.publish(&mut outcome)?;
            if pending.reply.send(Err(ControlError::Capacity)).is_err() {
                tracing::debug!(?token, "recording requester disconnected before admission");
            }
        }
        Ok(())
    }

    pub(super) fn close_recording_admissions(&mut self) {
        for (token, pending) in std::mem::take(&mut self.recording_admissions) {
            if pending
                .reply
                .send(Err(ControlError::Adapter("Control task stopped".into())))
                .is_err()
            {
                tracing::debug!(?token, "recording requester disconnected during shutdown");
            }
        }
    }
}
