use rusterisk_core::{
    Call, CallEvent, CallState, Transfer, TransferDecision, TransferError, TransferOutcome,
    TransferState, TransferToken,
};

fn participant(generation: u64, bridge: u64) -> Result<Call, String> {
    Call::outgoing(generation)
        .apply(CallEvent::Answer)
        .call()
        .join(bridge)
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn transfer_detaches_only_the_current_answered_pair_and_preserves_call_media() -> Result<(), String>
{
    let transferor = participant(11, 30)?.connect_media().call();
    let transferee = participant(12, 30)?.connect_media().call();
    let start = Transfer::begin(TransferToken::new(20), transferor, transferee)
        .map_err(|error| format!("{error:?}"))?;
    assert_eq!(start.release_bridge, 30);
    assert_eq!(start.transferor.generation(), 11);
    assert_eq!(start.transferee.generation(), 12);
    for (before, after) in [
        (transferor, start.transferor),
        (transferee, start.transferee),
    ] {
        assert_eq!(after.state(), CallState::Up);
        assert_eq!(after.bridge(), None);
        assert_eq!(after.media(), before.media());
        assert_eq!(after.media_connection(), before.media_connection());
        assert_eq!(after.audio_gate(), before.audio_gate());
        assert_eq!(after.stasis(), before.stasis());
    }
    assert_eq!(start.transfer.state(), TransferState::Proceeding);
    assert_eq!(
        Transfer::begin(TransferToken::new(21), start.transferor, start.transferee),
        Err(TransferError::Bridge)
    );
    Ok(())
}

#[test]
fn transfer_requires_distinct_live_participants_in_one_bridge() -> Result<(), String> {
    let token = TransferToken::new(20);
    let left = participant(11, 30)?;
    let right = participant(12, 30)?;
    assert_eq!(
        Transfer::begin(token, left, left),
        Err(TransferError::Participants)
    );
    assert_eq!(
        Transfer::begin(token, left, participant(12, 31)?),
        Err(TransferError::Bridge)
    );
    for call in [
        Call::outgoing(11),
        Call::outgoing(11).apply(CallEvent::Ring).call(),
        left.apply(CallEvent::Hangup).call(),
    ] {
        assert_eq!(
            Transfer::begin(token, call, right),
            Err(TransferError::NotAnswered)
        );
        assert_eq!(
            Transfer::begin(token, right, call),
            Err(TransferError::NotAnswered)
        );
    }
    Ok(())
}

#[test]
fn transfer_progress_survives_the_transferor_and_closes_only_its_own_monitor() -> Result<(), String>
{
    let token = TransferToken::new(20);
    let mut transfer = Transfer::begin(token, participant(11, 30)?, participant(12, 30)?)
        .map_err(|error| format!("{error:?}"))?
        .transfer;
    assert_eq!(
        transfer.participant_ended(token, 11),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(token, 13),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.finish(TransferToken::new(21), TransferOutcome::Succeeded),
        TransferDecision::Ignore
    );
    assert_eq!(transfer.state(), TransferState::Proceeding);
    assert_eq!(
        transfer.finish(token, TransferOutcome::Succeeded),
        TransferDecision::Finished {
            outcome: TransferOutcome::Succeeded,
            release: token
        }
    );
    assert_eq!(
        transfer.state(),
        TransferState::Finished(TransferOutcome::Succeeded)
    );
    assert_eq!(
        transfer.finish(token, TransferOutcome::Failed),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(token, 12),
        TransferDecision::Ignore
    );
    Ok(())
}

#[test]
fn ending_the_transferee_cancels_its_monitor_once() -> Result<(), String> {
    let token = TransferToken::new(20);
    let mut transfer = Transfer::begin(token, participant(11, 30)?, participant(12, 30)?)
        .map_err(|error| format!("{error:?}"))?
        .transfer;
    assert_eq!(
        transfer.participant_ended(TransferToken::new(21), 12),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(token, 12),
        TransferDecision::Finished {
            outcome: TransferOutcome::Cancelled,
            release: token
        }
    );
    assert_eq!(
        transfer.finish(token, TransferOutcome::Succeeded),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(token, 12),
        TransferDecision::Ignore
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Observation {
    Succeeded,
    Failed,
    Cancelled,
    TransferorEnded,
    TransfereeEnded,
    Stale,
}

#[test]
fn every_short_observation_sequence_releases_one_monitor_at_the_first_current_terminal_event(
) -> Result<(), String> {
    let observations = [
        Observation::Succeeded,
        Observation::Failed,
        Observation::Cancelled,
        Observation::TransferorEnded,
        Observation::TransfereeEnded,
        Observation::Stale,
    ];
    for generation in [0_u64, 1, u64::MAX] {
        let survivor = generation.wrapping_add(1);
        let token = TransferToken::new(generation);
        let other = TransferToken::new(generation ^ 1);
        for mut sequence in 0..observations.len().pow(4) {
            let mut transfer = Transfer::begin(
                token,
                participant(generation, 30)?,
                participant(survivor, 30)?,
            )
            .map_err(|error| format!("{error:?}"))?
            .transfer;
            let mut finished = None;
            let mut released = 0;
            for _ in 0..4 {
                let observation = observations[sequence % observations.len()];
                sequence /= observations.len();
                let expected = match observation {
                    Observation::Succeeded => Some(TransferOutcome::Succeeded),
                    Observation::Failed => Some(TransferOutcome::Failed),
                    Observation::Cancelled | Observation::TransfereeEnded => {
                        Some(TransferOutcome::Cancelled)
                    }
                    Observation::TransferorEnded | Observation::Stale => None,
                };
                let decision = match observation {
                    Observation::Succeeded => transfer.finish(token, TransferOutcome::Succeeded),
                    Observation::Failed => transfer.finish(token, TransferOutcome::Failed),
                    Observation::Cancelled => transfer.finish(token, TransferOutcome::Cancelled),
                    Observation::TransferorEnded => transfer.participant_ended(token, generation),
                    Observation::TransfereeEnded => transfer.participant_ended(token, survivor),
                    Observation::Stale => transfer.finish(other, TransferOutcome::Succeeded),
                };
                match (finished, expected) {
                    (None, Some(outcome)) => {
                        assert_eq!(
                            decision,
                            TransferDecision::Finished {
                                outcome,
                                release: token
                            }
                        );
                        finished = Some(outcome);
                        released += 1;
                    }
                    (None, None) | (Some(_), _) => assert_eq!(decision, TransferDecision::Ignore),
                }
                assert_eq!(
                    transfer.state(),
                    finished.map_or(TransferState::Proceeding, TransferState::Finished)
                );
            }
            assert_eq!(released, usize::from(finished.is_some()));
        }
    }
    Ok(())
}
