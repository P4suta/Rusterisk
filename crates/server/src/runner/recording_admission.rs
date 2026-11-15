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

#[cfg(test)]
mod tests {
    use super::super::Request;
    use super::Runner;
    use crate::{Action, ControlError, Controller, Credentials, Effect, Outcome, Signal};
    use rusterisk_api::ChannelId;
    use rusterisk_config::Config;
    use tokio::sync::{mpsc, oneshot};

    fn id(value: &str) -> Result<ChannelId, ControlError> {
        Ok(ChannelId::try_from(value.to_owned())?)
    }
    fn runner() -> Result<(Runner, mpsc::Receiver<Effect>), ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=password\n",
            "",
            "",
            "",
        )
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
        let (_controller, mut runner, effects) = Controller::new(config, 4);
        runner.control.subscribe("calls")?;
        runner.control.apply(
            Action::CreateBridge {
                id: id("mix")?,
                name: String::new(),
            },
            "now",
            0,
        )?;
        Ok((runner, effects))
    }
    fn record(
        runner: &mut Runner,
        now: u64,
    ) -> Result<oneshot::Receiver<Result<Outcome, ControlError>>, ControlError> {
        let (reply, response) = oneshot::channel();
        runner.request(
            Request::Apply {
                credentials: Credentials::new("app".into(), "password".into())?,
                origin: None,
                action: Action::RecordBridge {
                    bridge: id("mix")?,
                    name: id("saved")?,
                },
                reply,
            },
            now,
        )?;
        Ok(response)
    }

    #[test]
    fn admission_expires_at_five_seconds_and_late_success_cannot_restart_cancelled_audio(
    ) -> Result<(), ControlError> {
        let (mut runner, mut effects) = runner()?;
        let mut reply = record(&mut runner, 1000)?;
        let Effect::RecordMedia { owner, token, .. } =
            effects.try_recv().map_err(|_| ControlError::Capacity)?
        else {
            return Err(ControlError::Unsupported);
        };
        runner.expire_recording_admissions(5999)?;
        assert!(matches!(
            reply.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        runner.expire_recording_admissions(6000)?;
        assert!(matches!(
            reply.try_recv().map_err(|_| ControlError::Capacity)?,
            Err(ControlError::Capacity)
        ));
        assert!(
            matches!(effects.try_recv().map_err(|_| ControlError::Capacity)?, Effect::EndRecording { token: current, mode: crate::media::RecordingEnd::Cancel } if current == token)
        );
        runner.apply_signal(
            Signal::RecordingAdmitted {
                owner: owner.clone(),
                token,
                result: Ok(()),
            },
            6000,
        )?;
        let current = runner
            .control
            .apply(Action::GetRecording(id("saved")?), "now", 6000)?
            .body
            .ok_or(ControlError::Capacity)?;
        assert_eq!(current["state"], "canceled");
        runner.apply_signal(
            Signal::RecordingFinished {
                owner,
                token,
                result: Ok(()),
            },
            6000,
        )?;
        assert!(matches!(
            runner
                .control
                .apply(Action::GetRecording(id("saved")?), "now", 6000),
            Err(ControlError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn admission_clock_overflow_fails_before_reserving_a_recording() -> Result<(), ControlError> {
        let (mut runner, mut effects) = runner()?;
        let mut reply = record(&mut runner, u64::MAX - 4999)?;
        assert!(matches!(
            reply.try_recv().map_err(|_| ControlError::Capacity)?,
            Err(ControlError::Capacity)
        ));
        assert!(effects.try_recv().is_err());
        assert!(matches!(
            runner
                .control
                .apply(Action::GetRecording(id("saved")?), "now", 0),
            Err(ControlError::NotFound(_))
        ));
        Ok(())
    }
}
