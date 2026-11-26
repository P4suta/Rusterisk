use super::{call_pairs::ActivePair, CallKey, ChannelKind, Control, ControlError, Effect, Outcome};
use rusterisk_core::{AttendedTransfer, TransferDecision, TransferOutcome, TransferToken};
use rusterisk_sip::{AttendedReferSource, ReferFailure, ReferProgress, ReplacementDialog};

fn rejected(source: CallKey, refer: rusterisk_sip::ReferKey, failure: ReferFailure) -> Outcome {
    let mut outcome = Outcome::empty();
    outcome.effects.push(Effect::RejectRefer {
        source,
        refer,
        failure,
    });
    outcome
}

impl Control {
    pub(super) fn attended_transfer(
        &mut self,
        source: CallKey,
        refer: AttendedReferSource,
        consultation: ReplacementDialog,
        now: u64,
        timestamp: &str,
    ) -> Result<Outcome, ControlError> {
        let reference = refer.key();
        let Some(source_entry) = self.channels.by_key(&source) else {
            return Ok(rejected(source, reference, ReferFailure::NotFound));
        };
        if !matches!(source_entry.kind, ChannelKind::Sip)
            || source_entry.endpoint != refer.authority().endpoint()
        {
            return Ok(rejected(source, reference, ReferFailure::Forbidden));
        }
        let Some((consultation_id, consultation_entry)) =
            self.channels.iter().find(|(id, entry)| {
                id.as_str() == consultation.id()
                    && entry.call.generation() == consultation.generation()
            })
        else {
            return Ok(rejected(source, reference, ReferFailure::NotFound));
        };
        let consultation_key = CallKey {
            id: consultation_id.clone(),
            generation: consultation.generation(),
        };
        if !matches!(consultation_entry.kind, ChannelKind::Sip)
            || consultation_entry.endpoint != consultation.authority().endpoint()
        {
            return Ok(rejected(source, reference, ReferFailure::Forbidden));
        }
        if let Err(failure) = refer
            .authority()
            .replacement_authority(consultation.authority())
        {
            return Ok(rejected(source, reference, failure));
        }
        let source_pair = match self.transfer_dial_pair(&source) {
            Ok(pair) => pair,
            Err(ControlError::Unsupported) => {
                return Ok(rejected(source, reference, ReferFailure::Unsupported))
            }
            Err(error) => return Err(error),
        };
        let consultation_pair = match self.transfer_dial_pair(&consultation_key) {
            Ok(pair) => pair,
            Err(ControlError::Unsupported) => {
                return Ok(rejected(source, reference, ReferFailure::Unsupported))
            }
            Err(error) => return Err(error),
        };
        if self.call_pairs.len() >= self.capacity / 2 {
            return Ok(rejected(source, reference, ReferFailure::Unavailable));
        }
        let transferee = if source_pair.parent == source {
            source_pair.peer.clone()
        } else {
            source_pair.parent.clone()
        };
        let target = if consultation_pair.parent == consultation_key {
            consultation_pair.peer.clone()
        } else {
            consultation_pair.parent.clone()
        };
        let transferee_call = self
            .channels
            .by_handle(transferee.handle())
            .ok_or(ControlError::Conflict("Transfer survivor changed"))?
            .call;
        let target_call = self
            .channels
            .by_handle(target.handle())
            .ok_or(ControlError::Conflict("Consultation survivor changed"))?
            .call;
        let Some(bridge) = self.generation.checked_add(1) else {
            return Ok(rejected(source, reference, ReferFailure::Unavailable));
        };
        let token = TransferToken::new(bridge);
        let mut decision = match AttendedTransfer::begin(
            token,
            bridge,
            source_entry.call,
            transferee_call,
            consultation_entry.call,
            target_call,
        ) {
            Ok(decision) => decision,
            Err(_) => return Ok(rejected(source, reference, ReferFailure::Declined)),
        };
        self.detach_transfer_dial(&source_pair)?;
        self.detach_transfer_dial(&consultation_pair)?;
        self.generation = bridge;
        for (key, call) in [
            (&source, decision.transferor),
            (&transferee, decision.transferee),
            (&consultation_key, decision.consultation),
            (&target, decision.target),
        ] {
            self.channels
                .by_handle_mut(key.handle())
                .ok_or(ControlError::Conflict("Attended participant changed"))?
                .call = call;
        }
        self.call_pairs.insert(
            bridge,
            ActivePair {
                state: decision.pair,
                members: [transferee.clone(), target.clone()],
            },
        );
        let mut outcome = Outcome::empty();
        outcome.effects.push(Effect::ReferProgress {
            source: source.clone(),
            refer: reference,
            token,
            progress: ReferProgress::Trying,
        });
        for pair in [source_pair, consultation_pair] {
            outcome.effects.push(Effect::DialBridge {
                parent: pair.parent,
                generation: pair.bridge,
                peer: None,
            });
        }
        outcome.effects.push(Effect::DialBridge {
            parent: transferee,
            generation: bridge,
            peer: Some(target),
        });
        outcome.append(self.end_at(&source.id, timestamp, 16, now)?);
        outcome.append(self.end_at(&consultation_key.id, timestamp, 16, now)?);
        match decision.transfer.finish(token, TransferOutcome::Succeeded) {
            TransferDecision::Finished {
                release,
                outcome: TransferOutcome::Succeeded,
            } if release == token => {
                outcome.effects.push(Effect::ReferProgress {
                    source,
                    refer: reference,
                    token,
                    progress: ReferProgress::Answered,
                });
            }
            TransferDecision::Ignore | TransferDecision::Finished { .. } => {
                return Err(ControlError::Conflict(
                    "Attended transfer completion changed",
                ));
            }
        }
        Ok(outcome)
    }
}
