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

    pub(super) fn deadline(&self) -> u64 {
        self.deadline
    }
}

impl Runner {
    pub(crate) fn apply_signal(&mut self, signal: Signal, now: u64) -> Result<(), ControlError> {
        if self.reject_new_transfer(&signal)? {
            return Ok(());
        }
        if let Signal::ExternalRtpAdmitted { key, result } = signal {
            return self.complete_external_admission(key, result, now);
        }
        if let Signal::AudioSocketAdmitted { key, result } = signal {
            return self.complete_stream_admission(key, result, now);
        }
        if let Signal::WebSocketAdmitted { key, result } = signal {
            return self.complete_websocket_admission(key, result, now);
        }
        if let Signal::RecordingAdmitted { token, .. } = &signal {
            if self
                .recording_admissions
                .get(token)
                .is_some_and(|pending| pending.reply.is_closed() || now >= pending.deadline)
            {
                self.expire_recording_admission(*token)?;
            }
        }
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
            Signal::AttendedTransferRequested { .. }
            | Signal::BlindTransferRequested { .. }
            | Signal::EarlyMediaPrepared { .. }
            | Signal::MediaPlaybackStarted { .. }
            | Signal::MediaPlaybackFinished { .. }
            | Signal::PlaybackStarted { .. }
            | Signal::PlaybackFinished { .. }
            | Signal::DtmfReady { .. }
            | Signal::PeerDirection { .. }
            | Signal::EndpointContact { .. }
            | Signal::DtmfFinished { .. }
            | Signal::DtmfReceived { .. }
            | Signal::Milestone { .. }
            | Signal::Terminated { .. } => None,
            Signal::ExternalRtpAdmitted { .. } => None,
            Signal::AudioSocketAdmitted { .. } => None,
            Signal::WebSocketAdmitted { .. } => None,
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
        self.expire_recording_admissions(now)?;
        Ok(())
    }

    fn expire_recording_admission(
        &mut self,
        token: rusterisk_core::MediaTaskToken,
    ) -> Result<(), ControlError> {
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
            let pending_budget =
                MAX_PENDING_EFFECTS.saturating_sub(self.control.terminal_effect_reserve());
            let direct = self.effect_room() > 0;
            if !direct && self.pending_effects.len() >= pending_budget {
                break;
            }
            self.expire_recording_admission(token)?;
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
    use super::super::{Request, Subscriber};
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
                    options: Default::default(),
                },
                variables: Default::default(),
                reply,
            },
            now,
        )?;
        Ok(response)
    }

    #[tokio::test]
    async fn disconnected_calls_keep_release_capacity_when_recording_admissions_expire_together(
    ) -> Result<(), ControlError> {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=password\n",
            "",
            "",
            "",
        )
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
        let (_controller, mut runner, mut effects) = Controller::new(config, 64);
        runner.control.subscribe("calls")?;
        let mut replies = Vec::new();
        let mut keys = Vec::new();
        for index in 0..64 {
            let channel = id(&format!("call-{index}"))?;
            let bridge = id(&format!("bridge-{index}"))?;
            runner.control.incoming(
                crate::IncomingCall {
                    id: channel.clone(),
                    endpoint: "phone".into(),
                    caller: "100".into(),
                    context: "internal".into(),
                    extension: "100".into(),
                    target: crate::IncomingTarget::Stasis {
                        application: "calls".into(),
                        arguments: Vec::new(),
                    },
                },
                "now",
            )?;
            let key = runner.control.key(&channel)?;
            runner.control.signal(
                Signal::Milestone {
                    key: key.clone(),
                    milestone: crate::Milestone::Answered,
                },
                "now",
            )?;
            runner.control.apply(
                Action::CreateBridge {
                    id: bridge.clone(),
                    name: String::new(),
                },
                "now",
                0,
            )?;
            runner.control.apply(
                Action::AddChannels {
                    bridge: bridge.clone(),
                    channels: [channel].into_iter().collect(),
                },
                "now",
                0,
            )?;
            let (reply, response) = oneshot::channel();
            runner.request(
                Request::Apply {
                    credentials: Credentials::new("app".into(), "password".into())?,
                    origin: None,
                    action: Action::RecordBridge {
                        bridge,
                        name: id(&format!("recording-{index}"))?,
                        options: Default::default(),
                    },
                    variables: Default::default(),
                    reply,
                },
                1000,
            )?;
            replies.push(response);
            keys.push(key);
        }
        for key in keys {
            runner
                .effect_sender()?
                .try_send(Effect::Ring { key })
                .map_err(|_| ControlError::Capacity)?;
        }
        runner.expire_recording_admissions(6000)?;
        let (events, receiver) = mpsc::channel(64);
        drop(receiver);
        runner
            .subscribers
            .insert("calls".into(), Subscriber { events });
        runner.settle(6000)?;
        assert!(runner.pending_effects.len() <= super::super::MAX_PENDING_EFFECTS);
        assert!(runner.recording_admissions.len() < 64);
        assert_eq!(
            runner
                .control
                .apply(Action::ListChannels, "now", 6000)?
                .body,
            Some(serde_json::json!([]))
        );
        let drain = tokio::spawn(async move {
            let mut releases = 0;
            let mut bridges = 0;
            let mut cancellations = 0;
            let mut recordings = Vec::new();
            for _ in 0..320 {
                match effects.recv().await.ok_or(ControlError::Capacity)? {
                    Effect::Release { .. } => releases += 1,
                    Effect::Bridge { channels, .. } => {
                        assert!(channels.is_empty());
                        bridges += 1;
                    }
                    Effect::EndRecording {
                        mode: crate::media::RecordingEnd::Cancel,
                        ..
                    } => cancellations += 1,
                    Effect::RecordMedia { owner, token, .. } => recordings.push((owner, token)),
                    Effect::Ring { .. } => {}
                    Effect::RejectRefer { .. }
                    | Effect::ReferProgress { .. }
                    | Effect::ControlRecording { .. }
                    | Effect::ReserveWebSocket { .. }
                    | Effect::EndRecording {
                        mode: crate::media::RecordingEnd::Finish,
                        ..
                    }
                    | Effect::PlayMedia { .. }
                    | Effect::StartMusic { .. }
                    | Effect::PrepareEarlyMusic { .. }
                    | Effect::MusicClassUnavailable { .. }
                    | Effect::ControlPlayback { .. }
                    | Effect::Playback { .. }
                    | Effect::Dtmf { .. }
                    | Effect::Gate { .. }
                    | Effect::PeerDirection { .. }
                    | Effect::Answer { .. }
                    | Effect::Start { .. }
                    | Effect::ReleaseBridge { .. }
                    | Effect::DialBridge { .. }
                    | Effect::ConnectExternalRtp { .. }
                    | Effect::ConnectAudioSocket { .. } => return Err(ControlError::Unsupported),
                    Effect::ConnectWebSocket { .. } => return Err(ControlError::Unsupported),
                }
            }
            Ok((releases, bridges, cancellations, recordings))
        });
        runner
            .drain_effects(tokio::time::Instant::now() + std::time::Duration::from_secs(2))
            .await?;
        runner.expire_recording_admissions(6000)?;
        runner
            .drain_effects(tokio::time::Instant::now() + std::time::Duration::from_secs(2))
            .await?;
        let (releases, bridges, cancellations, recordings) =
            tokio::time::timeout(std::time::Duration::from_secs(2), drain)
                .await
                .map_err(|error| ControlError::Adapter(error.to_string()))?
                .map_err(|error| ControlError::Adapter(error.to_string()))??;
        assert_eq!((releases, bridges, cancellations), (64, 64, 64));
        for mut reply in replies {
            assert!(matches!(
                reply.try_recv().map_err(|_| ControlError::Capacity)?,
                Err(ControlError::Capacity)
            ));
        }
        for (owner, token) in recordings {
            runner.apply_signal(
                Signal::RecordingFinished {
                    owner,
                    token,
                    result: Ok(()),
                },
                6000,
            )?;
        }
        Ok(())
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
            matches!(effects.try_recv().map_err(|_| ControlError::Capacity)?, Effect::EndRecording { token: current, mode: crate::media::RecordingEnd::Cancel, .. } if current == token)
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
