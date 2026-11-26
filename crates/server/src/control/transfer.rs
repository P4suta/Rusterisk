use super::{CallKey, Control, ControlError, Effect, Outcome};
use rusterisk_core::{
    DialFailure, DialplanToken, Transfer, TransferDecision, TransferOutcome, TransferToken,
};
use rusterisk_sip::{DialogRefer, ReferFailure, ReferKey, ReferProgress};

pub(super) struct ActiveTransfer {
    source: CallKey,
    reference: ReferKey,
    survivor: CallKey,
    state: Transfer,
    stage: Stage,
}

enum Stage {
    HandedOff,
    Dialing(DialplanToken),
}

fn rejected(source: CallKey, refer: ReferKey, failure: ReferFailure) -> Outcome {
    let mut outcome = Outcome::empty();
    outcome.effects.push(Effect::RejectRefer {
        source,
        refer,
        failure,
    });
    outcome
}

fn failed(
    source: CallKey,
    refer: ReferKey,
    token: TransferToken,
    failure: ReferFailure,
) -> Outcome {
    let mut outcome = Outcome::empty();
    outcome.effects.push(Effect::ReferProgress {
        source,
        refer,
        token,
        progress: ReferProgress::Failed(failure),
    });
    outcome
}

impl Control {
    pub(super) fn blind_transfer(
        &mut self,
        source: CallKey,
        refer: DialogRefer,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let reference = refer.key();
        let Some(entry) = self.channels.by_key(&source) else {
            return Ok(rejected(source, reference, ReferFailure::NotFound));
        };
        if !matches!(entry.kind, super::ChannelKind::Sip)
            || entry.endpoint != refer.authority().endpoint()
        {
            return Ok(rejected(source, reference, ReferFailure::Forbidden));
        }
        let pair = match self.transfer_dial_pair(&source) {
            Ok(pair) => pair,
            Err(ControlError::Unsupported) => {
                return Ok(rejected(source, reference, ReferFailure::Unsupported))
            }
            Err(error) => return Err(error),
        };
        if self.transfers.len() >= self.capacity {
            return Ok(rejected(source, reference, ReferFailure::Unavailable));
        }
        let survivor = if pair.parent == source {
            pair.peer.clone()
        } else {
            pair.parent.clone()
        };
        let context = entry
            .variables
            .get("TRANSFER_CONTEXT")
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .unwrap_or_else(|| refer.authority().context());
        let extension = refer.target().extension();
        let frame = entry
            .dialplan
            .as_ref()
            .ok_or(ControlError::Conflict("Transfer settings unavailable"))?;
        let found = context.len() <= 128
            && extension.len() <= 128
            && !context
                .chars()
                .chain(extension.chars())
                .any(char::is_control)
            && frame.program().position(context, extension, 1).is_some();
        let (program, context, extension, source_call) = (
            frame.program().clone(),
            context.to_owned(),
            extension.to_owned(),
            entry.call,
        );
        let token = match self.generation() {
            Ok(generation) => TransferToken::new(generation),
            Err(ControlError::Capacity) => {
                return Ok(rejected(source, reference, ReferFailure::Unavailable))
            }
            Err(error) => return Err(error),
        };
        if !found {
            return Ok(failed(source, reference, token, ReferFailure::NotFound));
        }
        let survivor_call = self
            .channels
            .by_handle(survivor.handle())
            .ok_or(ControlError::Conflict("Transfer survivor changed"))?
            .call;
        let decision = match Transfer::begin(token, source_call, survivor_call) {
            Ok(decision) => decision,
            Err(_) => return Ok(rejected(source, reference, ReferFailure::Declined)),
        };
        if decision.release_bridge != pair.bridge {
            return Err(ControlError::Conflict("Transfer bridge changed"));
        }
        self.detach_transfer_dial(&pair)?;
        self.channels
            .by_handle_mut(source.handle())
            .ok_or(ControlError::NotFound("Transfer source disappeared"))?
            .call = decision.transferor;
        self.channels
            .by_handle_mut(survivor.handle())
            .ok_or(ControlError::NotFound("Transfer survivor disappeared"))?
            .call = decision.transferee;
        self.transfers.insert(
            token,
            ActiveTransfer {
                source: source.clone(),
                reference,
                survivor: survivor.clone(),
                state: decision.transfer,
                stage: Stage::HandedOff,
            },
        );
        let mut outcome = Outcome::empty();
        outcome.effects.push(Effect::ReferProgress {
            source: source.clone(),
            refer: reference,
            token,
            progress: ReferProgress::Trying,
        });
        outcome.effects.push(Effect::DialBridge {
            parent: pair.parent,
            generation: pair.bridge,
            peer: None,
        });
        outcome.append(self.end_at(&source.id, timestamp, 16, now)?);
        let entry = self
            .channels
            .by_handle_mut(survivor.handle())
            .ok_or(ControlError::Conflict("Transfer survivor changed"))?;
        entry.snapshot.dialplan.context = context;
        entry.snapshot.dialplan.exten = extension;
        entry.snapshot.dialplan.priority = 1;
        entry.snapshot.dialplan.app_name.clear();
        entry.snapshot.dialplan.app_data.clear();
        entry.dialplan = Some(super::DialplanFrame::dormant(program.clone()));
        match self.start_dialplan(&survivor, program, now, timestamp) {
            Ok(started) => {
                outcome.effects.push(Effect::ReferProgress {
                    source,
                    refer: reference,
                    token,
                    progress: ReferProgress::HandedOff,
                });
                outcome.append(started);
            }
            Err(_) => {
                outcome.append(self.finish_transfer(
                    token,
                    TransferOutcome::Failed,
                    ReferProgress::Failed(ReferFailure::Unavailable),
                ));
                if self
                    .channels
                    .by_handle(survivor.handle())
                    .is_some_and(|entry| entry.call.generation() == survivor.generation)
                {
                    outcome.append(self.end_at(&survivor.id, timestamp, 21, now)?);
                }
            }
        }
        Ok(outcome)
    }

    pub(super) fn transfer_dial_started(&mut self, parent: &CallKey, token: DialplanToken) {
        for transfer in self.transfers.values_mut() {
            if transfer.survivor == *parent && matches!(transfer.stage, Stage::HandedOff) {
                transfer.stage = Stage::Dialing(token);
            }
        }
    }

    fn transfer_for_dial(&self, parent: &CallKey, dial: DialplanToken) -> Option<TransferToken> {
        self.transfers.iter().find_map(|(token, transfer)| {
            (transfer.survivor == *parent
                && matches!(transfer.stage, Stage::Dialing(current) if current == dial))
            .then_some(*token)
        })
    }

    pub(super) fn transfer_dial_ringing(&self, parent: &CallKey, dial: DialplanToken) -> Outcome {
        let mut outcome = Outcome::empty();
        if let Some(transfer) = self
            .transfer_for_dial(parent, dial)
            .and_then(|token| self.transfers.get(&token).map(|transfer| (token, transfer)))
        {
            outcome.effects.push(Effect::ReferProgress {
                source: transfer.1.source.clone(),
                refer: transfer.1.reference,
                token: transfer.0,
                progress: ReferProgress::Ringing,
            });
        }
        outcome
    }

    pub(super) fn transfer_dial_answered(
        &mut self,
        parent: &CallKey,
        dial: DialplanToken,
    ) -> Outcome {
        self.transfer_for_dial(parent, dial)
            .map_or_else(Outcome::empty, |token| {
                self.finish_transfer(token, TransferOutcome::Succeeded, ReferProgress::Answered)
            })
    }

    pub(super) fn transfer_dial_failed(
        &mut self,
        parent: &CallKey,
        dial: DialplanToken,
        failure: DialFailure,
    ) -> Outcome {
        let failure = match failure {
            DialFailure::Busy => ReferFailure::Busy,
            DialFailure::NoAnswer => ReferFailure::Timeout,
            DialFailure::Congestion | DialFailure::Unavailable => ReferFailure::Unavailable,
        };
        self.transfer_for_dial(parent, dial)
            .map_or_else(Outcome::empty, |token| {
                self.finish_transfer(
                    token,
                    TransferOutcome::Failed,
                    ReferProgress::Failed(failure),
                )
            })
    }

    fn finish_transfer(
        &mut self,
        token: TransferToken,
        result: TransferOutcome,
        progress: ReferProgress,
    ) -> Outcome {
        let Some(transfer) = self.transfers.get_mut(&token) else {
            return Outcome::empty();
        };
        let decision = transfer.state.finish(token, result);
        self.apply_transfer_decision(decision, progress)
    }

    fn apply_transfer_decision(
        &mut self,
        decision: TransferDecision,
        progress: ReferProgress,
    ) -> Outcome {
        let mut outcome = Outcome::empty();
        if let TransferDecision::Finished { release, .. } = decision {
            if let Some(transfer) = self.transfers.remove(&release) {
                outcome.effects.push(Effect::ReferProgress {
                    source: transfer.source,
                    refer: transfer.reference,
                    token: release,
                    progress,
                });
            }
        }
        outcome
    }

    pub(super) fn end_transfer_participant(&mut self, key: &CallKey) -> Outcome {
        let decisions: Vec<_> = self
            .transfers
            .iter_mut()
            .map(|(token, transfer)| transfer.state.participant_ended(*token, key.generation))
            .collect();
        let mut outcome = Outcome::empty();
        for decision in decisions {
            outcome.append(
                self.apply_transfer_decision(
                    decision,
                    ReferProgress::Failed(ReferFailure::Cancelled),
                ),
            );
        }
        outcome
    }

    pub(super) fn transfer_terminal_effects(&self, key: &CallKey) -> usize {
        self.transfers
            .values()
            .filter(|transfer| transfer.survivor == *key)
            .count()
    }
}
