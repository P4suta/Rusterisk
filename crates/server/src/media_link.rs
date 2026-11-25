use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, PeerMediaDirection, Wake};
use rusterisk_media::{MediaError, PcmFrame, MAX_SAMPLES};
use tokio::sync::{mpsc, watch};

pub struct Channels<const N: usize = MAX_SAMPLES> {
    pub incoming: mpsc::Sender<PcmFrame<N>>,
    pub digits: mpsc::Sender<DtmfTone>,
    pub outgoing: mpsc::Receiver<PcmFrame<N>>,
    pub commands: mpsc::Receiver<DigitCommand>,
    pub completed: mpsc::Sender<DtmfToken>,
    pub stop: watch::Receiver<bool>,
    pub direction: watch::Receiver<PeerMediaDirection>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkStats {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub dropped: u64,
}

pub enum DigitCommand {
    Tone(DtmfTone),
    Sequence(DtmfCommand),
}

#[derive(Default)]
pub(crate) struct DigitSchedule {
    active: Option<DigitCommand>,
}

pub(crate) enum DigitStep {
    Waiting,
    Send(DtmfTone),
    Finished(DtmfToken),
}

impl DigitSchedule {
    pub(crate) fn idle(&self) -> bool {
        self.active.is_none()
    }

    pub(crate) fn admit(&mut self, command: DigitCommand) -> Result<(), MediaError> {
        if self.active.is_some() {
            return Err(MediaError::Capacity);
        }
        self.active = Some(command);
        Ok(())
    }

    pub(crate) fn wake(&self) -> Wake {
        match &self.active {
            Some(DigitCommand::Sequence(command)) => command.sequence.wake(),
            Some(DigitCommand::Tone(_)) => Wake::Now,
            None => Wake::Idle,
        }
    }

    pub(crate) fn cancel(&mut self) -> Option<DtmfToken> {
        match self.active.take() {
            Some(DigitCommand::Sequence(command)) => Some(command.token),
            Some(DigitCommand::Tone(_)) | None => None,
        }
    }

    pub(crate) fn advance(&mut self, now: u64, ready: bool) -> Result<DigitStep, MediaError> {
        match &mut self.active {
            Some(DigitCommand::Tone(tone)) if ready => {
                let tone = *tone;
                self.active = None;
                Ok(DigitStep::Send(tone))
            }
            Some(DigitCommand::Sequence(command)) => {
                let mut sequence = command.sequence;
                let tone = sequence.advance(now).map_err(|error| {
                    MediaError::Transport(format!("Invalid digit schedule: {error:?}"))
                })?;
                if tone.is_some() && !ready {
                    return Ok(DigitStep::Waiting);
                }
                command.sequence = sequence;
                if sequence.finished() {
                    let token = command.token;
                    self.active = None;
                    Ok(DigitStep::Finished(token))
                } else {
                    Ok(tone.map_or(DigitStep::Waiting, DigitStep::Send))
                }
            }
            Some(DigitCommand::Tone(_)) | None => Ok(DigitStep::Waiting),
        }
    }
}
