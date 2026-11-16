use crate::{Call, CallState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransferToken(u64);

impl TransferToken {
    pub const fn new(generation: u64) -> Self {
        Self(generation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferState {
    Proceeding,
    Finished(TransferOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferError {
    Participants,
    NotAnswered,
    Bridge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDecision {
    Ignore,
    Finished {
        outcome: TransferOutcome,
        release: TransferToken,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    token: TransferToken,
    transferee: u64,
    state: TransferState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferStart {
    pub transfer: Transfer,
    pub transferor: Call,
    pub transferee: Call,
    pub release_bridge: u64,
}

impl Transfer {
    pub fn begin(
        token: TransferToken,
        transferor: Call,
        transferee: Call,
    ) -> Result<TransferStart, TransferError> {
        if transferor.generation() == transferee.generation() {
            return Err(TransferError::Participants);
        }
        if transferor.state() != CallState::Up || transferee.state() != CallState::Up {
            return Err(TransferError::NotAnswered);
        }
        let bridge = transferor.bridge().ok_or(TransferError::Bridge)?;
        if transferee.bridge() != Some(bridge) {
            return Err(TransferError::Bridge);
        }
        Ok(TransferStart {
            transfer: Self {
                token,
                transferee: transferee.generation(),
                state: TransferState::Proceeding,
            },
            transferor: transferor
                .leave(bridge)
                .map_err(|_error| TransferError::Bridge)?,
            transferee: transferee
                .leave(bridge)
                .map_err(|_error| TransferError::Bridge)?,
            release_bridge: bridge,
        })
    }

    pub const fn state(self) -> TransferState {
        self.state
    }

    pub fn finish(&mut self, token: TransferToken, outcome: TransferOutcome) -> TransferDecision {
        if token != self.token || !matches!(self.state, TransferState::Proceeding) {
            return TransferDecision::Ignore;
        }
        self.state = TransferState::Finished(outcome);
        TransferDecision::Finished {
            outcome,
            release: self.token,
        }
    }

    pub fn participant_ended(&mut self, token: TransferToken, generation: u64) -> TransferDecision {
        if generation == self.transferee {
            self.finish(token, TransferOutcome::Cancelled)
        } else {
            TransferDecision::Ignore
        }
    }
}
