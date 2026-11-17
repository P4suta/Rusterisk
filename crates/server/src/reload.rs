use crate::ControlError;
use rusterisk_core::{SettingsCandidate, SettingsGeneration};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub(crate) const PUBLICATION_BUDGET: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublicationKey {
    candidate: SettingsCandidate,
    base: SettingsGeneration,
}

impl PublicationKey {
    pub(crate) fn new(candidate: SettingsCandidate, base: SettingsGeneration) -> Self {
        Self { candidate, base }
    }

    pub(crate) fn candidate(self) -> SettingsCandidate {
        self.candidate
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationPhase {
    Staged,
    Quiesce,
    Publish(SettingsGeneration),
    Resume(SettingsGeneration),
    Abort,
}

struct PublicationAck {
    key: PublicationKey,
    phase: PublicationPhase,
}

pub(crate) struct Acknowledgement {
    key: PublicationKey,
    phase: PublicationPhase,
    receiver: oneshot::Receiver<Result<PublicationAck, ControlError>>,
}

impl Acknowledgement {
    pub(crate) async fn receive(self) -> Result<(), ControlError> {
        let acknowledgement = self
            .receiver
            .await
            .map_err(|_| ControlError::Adapter("Settings participant stopped".into()))??;
        if acknowledgement.key != self.key || acknowledgement.phase != self.phase {
            return Err(ControlError::Conflict("Settings acknowledgement changed"));
        }
        Ok(())
    }
}

pub(crate) struct PublicationReply {
    key: PublicationKey,
    phase: PublicationPhase,
    sender: oneshot::Sender<Result<PublicationAck, ControlError>>,
}

impl PublicationReply {
    pub(crate) fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    pub(crate) fn complete(self, result: Result<(), ControlError>) {
        let result = result.map(|()| PublicationAck {
            key: self.key,
            phase: self.phase,
        });
        if self.sender.send(result).is_err() {
            tracing::debug!("Settings coordinator disconnected");
        }
    }
}

fn response(key: PublicationKey, phase: PublicationPhase) -> (PublicationReply, Acknowledgement) {
    let (sender, receiver) = oneshot::channel();
    (
        PublicationReply { key, phase, sender },
        Acknowledgement {
            key,
            phase,
            receiver,
        },
    )
}

pub(crate) struct PreparedHandoff<P> {
    pub(crate) key: PublicationKey,
    pub(crate) payload: P,
    pub(crate) reply: PublicationReply,
}

pub(crate) struct PublicationCommand {
    pub(crate) key: PublicationKey,
    pub(crate) phase: PublicationPhase,
    pub(crate) reply: PublicationReply,
}

pub(crate) struct PublicationPort<P> {
    prepared: mpsc::Sender<PreparedHandoff<P>>,
    commands: mpsc::Sender<PublicationCommand>,
}

pub(crate) struct PublicationReceiver<P> {
    pub(crate) prepared: mpsc::Receiver<PreparedHandoff<P>>,
    pub(crate) commands: mpsc::Receiver<PublicationCommand>,
}

impl<P> Clone for PublicationPort<P> {
    fn clone(&self) -> Self {
        Self {
            prepared: self.prepared.clone(),
            commands: self.commands.clone(),
        }
    }
}

impl<P> PublicationPort<P> {
    pub(crate) fn channel() -> (Self, PublicationReceiver<P>) {
        let (prepared, preparation_receiver) = mpsc::channel(1);
        let (commands, command_receiver) = mpsc::channel(4);
        (
            Self { prepared, commands },
            PublicationReceiver {
                prepared: preparation_receiver,
                commands: command_receiver,
            },
        )
    }

    pub(crate) fn stage(
        &self,
        key: PublicationKey,
        payload: P,
    ) -> Result<Acknowledgement, ControlError> {
        let (reply, acknowledgement) = response(key, PublicationPhase::Staged);
        self.prepared
            .try_send(PreparedHandoff {
                key,
                payload,
                reply,
            })
            .map_err(|_| ControlError::Capacity)?;
        Ok(acknowledgement)
    }

    pub(crate) fn command(
        &self,
        key: PublicationKey,
        phase: PublicationPhase,
    ) -> Result<Acknowledgement, ControlError> {
        if phase == PublicationPhase::Staged {
            return Err(ControlError::Invalid("Settings payload is required"));
        }
        let (reply, acknowledgement) = response(key, phase);
        self.commands
            .try_send(PublicationCommand { key, phase, reply })
            .map_err(|_| ControlError::Capacity)?;
        Ok(acknowledgement)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StagedPhase {
    Ready,
    Quiesced,
    Published(SettingsGeneration),
}

struct Staged<P> {
    key: PublicationKey,
    payload: Option<P>,
    phase: StagedPhase,
}

pub(crate) struct PublicationState<P> {
    current: SettingsGeneration,
    staged: Option<Staged<P>>,
}

impl<P> PublicationState<P> {
    pub(crate) fn new(current: SettingsGeneration) -> Self {
        Self {
            current,
            staged: None,
        }
    }

    pub(crate) fn admitting(&self) -> bool {
        self.staged
            .as_ref()
            .is_none_or(|staged| staged.phase == StagedPhase::Ready)
    }

    pub(crate) fn stage(&mut self, key: PublicationKey, payload: P) -> Result<(), ControlError> {
        if key.base != self.current || !key.candidate.follows(key.base) {
            return Err(ControlError::Conflict("Settings base generation changed"));
        }
        if self.staged.is_some() {
            return Err(ControlError::Capacity);
        }
        self.staged = Some(Staged {
            key,
            payload: Some(payload),
            phase: StagedPhase::Ready,
        });
        Ok(())
    }

    fn pending(&mut self, key: PublicationKey) -> Result<&mut Staged<P>, ControlError> {
        self.staged
            .as_mut()
            .filter(|staged| staged.key == key)
            .ok_or(ControlError::Conflict("Unknown settings publication"))
    }

    pub(crate) fn prepared(&self, key: PublicationKey) -> Result<&P, ControlError> {
        self.staged
            .as_ref()
            .filter(|staged| staged.key == key && staged.phase == StagedPhase::Ready)
            .and_then(|staged| staged.payload.as_ref())
            .ok_or(ControlError::Conflict("Prepared settings are unavailable"))
    }

    pub(crate) fn quiesce(&mut self, key: PublicationKey) -> Result<(), ControlError> {
        let staged = self.pending(key)?;
        match staged.phase {
            StagedPhase::Ready | StagedPhase::Quiesced => {
                staged.phase = StagedPhase::Quiesced;
                Ok(())
            }
            StagedPhase::Published(_) => {
                Err(ControlError::Conflict("Settings are already published"))
            }
        }
    }

    pub(crate) fn publish(
        &mut self,
        key: PublicationKey,
        generation: SettingsGeneration,
    ) -> Result<P, ControlError> {
        let staged = self.pending(key)?;
        if staged.phase != StagedPhase::Quiesced || !generation.matches_candidate(key.candidate) {
            return Err(ControlError::Conflict(
                "Settings publication is out of order",
            ));
        }
        let payload = staged
            .payload
            .take()
            .ok_or(ControlError::Conflict("Settings payload is missing"))?;
        staged.phase = StagedPhase::Published(generation);
        Ok(payload)
    }

    pub(crate) fn resume(
        &mut self,
        key: PublicationKey,
        generation: SettingsGeneration,
    ) -> Result<(), ControlError> {
        if self.pending(key)?.phase != StagedPhase::Published(generation) {
            return Err(ControlError::Conflict("Settings resume generation changed"));
        }
        self.current = generation;
        self.staged = None;
        Ok(())
    }

    pub(crate) fn abort(&mut self, key: PublicationKey) -> Result<(), ControlError> {
        if self.staged.is_none() && self.current == key.base {
            return Ok(());
        }
        if matches!(self.pending(key)?.phase, StagedPhase::Published(_)) {
            return Err(ControlError::Conflict(
                "Published settings require shutdown",
            ));
        }
        self.staged = None;
        Ok(())
    }
}
