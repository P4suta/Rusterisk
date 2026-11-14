use crate::{CallState, DialplanError, MediaConnection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerStatus {
    Pending,
    Ready,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Answering,
    Media { deadline: u64 },
    Ready,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnswerProgress {
    phase: Phase,
}

impl Default for AnswerProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl AnswerProgress {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Answering,
        }
    }

    pub fn cancel(&mut self) {
        self.phase = Phase::Cancelled;
    }

    pub fn observe(
        &mut self,
        now: u64,
        call: CallState,
        media: MediaConnection,
    ) -> Result<AnswerStatus, DialplanError> {
        if call == CallState::Ended {
            self.cancel();
        }
        self.phase = match self.phase {
            Phase::Answering if call == CallState::Up => {
                if media == MediaConnection::Connected {
                    Phase::Ready
                } else {
                    Phase::Media {
                        deadline: now.checked_add(500).ok_or(DialplanError::Clock)?,
                    }
                }
            }
            Phase::Media { deadline } if media == MediaConnection::Connected || now >= deadline => {
                Phase::Ready
            }
            Phase::Answering | Phase::Media { .. } | Phase::Ready | Phase::Cancelled => self.phase,
        };
        Ok(match self.phase {
            Phase::Answering | Phase::Media { .. } => AnswerStatus::Pending,
            Phase::Ready => AnswerStatus::Ready,
            Phase::Cancelled => AnswerStatus::Cancelled,
        })
    }
}
