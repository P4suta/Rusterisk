use crate::{
    Call, CallPairError, CallState, OwnedCallPair, TransferDecision, TransferError,
    TransferOutcome, TransferState, TransferToken,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttendedTransfer {
    token: TransferToken,
    survivors: [u64; 2],
    state: TransferState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttendedStart {
    pub transfer: AttendedTransfer,
    pub pair: OwnedCallPair,
    pub transferor: Call,
    pub transferee: Call,
    pub consultation: Call,
    pub target: Call,
    pub release_bridges: [u64; 2],
}

impl AttendedTransfer {
    pub fn begin(
        token: TransferToken,
        bridge: u64,
        transferor: Call,
        transferee: Call,
        consultation: Call,
        target: Call,
    ) -> Result<AttendedStart, TransferError> {
        let calls = [transferor, transferee, consultation, target];
        for (index, call) in calls.iter().enumerate() {
            if calls[..index]
                .iter()
                .any(|previous| previous.generation() == call.generation())
            {
                return Err(TransferError::Participants);
            }
        }
        if calls.iter().any(|call| call.state() != CallState::Up) {
            return Err(TransferError::NotAnswered);
        }
        let first = transferor.bridge().ok_or(TransferError::Bridge)?;
        let second = consultation.bridge().ok_or(TransferError::Bridge)?;
        if first == second
            || transferee.bridge() != Some(first)
            || target.bridge() != Some(second)
            || [first, second].contains(&bridge)
        {
            return Err(TransferError::Bridge);
        }
        let transferor = transferor.leave(first).map_err(|_| TransferError::Bridge)?;
        let consultation = consultation
            .leave(second)
            .map_err(|_| TransferError::Bridge)?;
        let transferee = transferee
            .leave(first)
            .and_then(|call| call.join(bridge))
            .map_err(|_| TransferError::Bridge)?;
        let target = target
            .leave(second)
            .and_then(|call| call.join(bridge))
            .map_err(|_| TransferError::Bridge)?;
        let pair = OwnedCallPair::new(transferee, target).map_err(|error| match error {
            CallPairError::Participants => TransferError::Participants,
            CallPairError::NotAnswered => TransferError::NotAnswered,
            CallPairError::Bridge => TransferError::Bridge,
        })?;
        Ok(AttendedStart {
            transfer: Self {
                token,
                survivors: [transferee.generation(), target.generation()],
                state: TransferState::Proceeding,
            },
            pair,
            transferor,
            transferee,
            consultation,
            target,
            release_bridges: [first, second],
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
        if self.survivors.contains(&generation) {
            self.finish(token, TransferOutcome::Cancelled)
        } else {
            TransferDecision::Ignore
        }
    }
}
