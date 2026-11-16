use rusterisk_core::{
    AttendedStart, AttendedTransfer, Call, CallEvent, CallPairState, MuteDirection,
    PeerMediaDirection, StasisApplication, TransferDecision, TransferError, TransferOutcome,
    TransferState, TransferToken,
};

fn failure(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

fn participant(generation: u64, bridge: u64) -> Result<Call, String> {
    Call::outgoing(generation)
        .apply(CallEvent::Answer)
        .call()
        .join(bridge)
        .map_err(failure)
}

fn participants(generations: [u64; 4]) -> Result<[Call; 4], String> {
    Ok([
        participant(generations[0], 30)?,
        participant(generations[1], 30)?,
        participant(generations[2], 31)?,
        participant(generations[3], 31)?,
    ])
}

fn begin(
    calls: [Call; 4],
    bridge: u64,
    token: TransferToken,
) -> Result<AttendedStart, TransferError> {
    let [transferor, transferee, consultation, target] = calls;
    AttendedTransfer::begin(token, bridge, transferor, transferee, consultation, target)
}

fn preserved(before: Call, after: Call) -> Result<(), String> {
    assert_eq!(after.generation(), before.generation());
    assert_eq!(after.state(), before.state());
    assert_eq!(after.media(), before.media());
    assert_eq!(after.media_connection(), before.media_connection());
    assert_eq!(after.connection_claim(), before.connection_claim());
    assert_eq!(after.peer_direction(), before.peer_direction());
    assert_eq!(after.audio_gate(), before.audio_gate());
    assert_eq!(after.stasis(), before.stasis());
    assert_eq!(
        after
            .with_peer_direction(PeerMediaDirection::SendReceive)
            .map_err(failure)?
            .audio_gate(),
        before
            .with_peer_direction(PeerMediaDirection::SendReceive)
            .map_err(failure)?
            .audio_gate(),
    );
    Ok(())
}

#[test]
fn handoff_joins_the_existing_survivors_and_preserves_each_call_property() -> Result<(), String> {
    for role in 0..4 {
        for flags in 0..32 {
            for direction in [
                PeerMediaDirection::SendReceive,
                PeerMediaDirection::SendOnly,
                PeerMediaDirection::ReceiveOnly,
                PeerMediaDirection::Inactive,
            ] {
                let mut calls = participants([11, 12, 13, 14])?;
                let mut changed = calls[role];
                if flags & 1 != 0 {
                    changed = changed.connect_media().call();
                }
                if flags & 2 != 0 {
                    changed = changed.hold().map_err(failure)?;
                }
                if flags & 4 != 0 {
                    changed = changed.mute(MuteDirection::Both).map_err(failure)?;
                }
                if flags & 8 != 0 {
                    changed = changed.claim_connection().map_err(failure)?;
                }
                if flags & 16 != 0 {
                    changed = changed
                        .enter_stasis(StasisApplication::new(70))
                        .map_err(failure)?;
                }
                calls[role] = changed.with_peer_direction(direction).map_err(failure)?;
                let start = begin(calls, 40, TransferToken::new(50)).map_err(failure)?;
                assert_eq!(start.release_bridges, [30, 31]);
                assert_eq!(start.transferor.bridge(), None);
                assert_eq!(start.consultation.bridge(), None);
                assert_eq!(start.transferee.bridge(), Some(40));
                assert_eq!(start.target.bridge(), Some(40));
                assert_eq!(start.pair.bridge(), 40);
                assert_eq!(start.pair.generations(), [12, 14]);
                assert_eq!(start.pair.state(), CallPairState::Linked);
                assert_eq!(start.transfer.state(), TransferState::Proceeding);
                for (before, after) in calls.into_iter().zip([
                    start.transferor,
                    start.transferee,
                    start.consultation,
                    start.target,
                ]) {
                    preserved(before, after)?;
                }
            }
        }
    }
    Ok(())
}

#[test]
fn every_overlap_between_the_four_participant_generations_is_rejected() -> Result<(), String> {
    let token = TransferToken::new(50);
    for left in 0..4 {
        for right in left + 1..4 {
            let mut generations = [11, 12, 13, 14];
            generations[right] = generations[left];
            assert_eq!(
                begin(participants(generations)?, 40, token),
                Err(TransferError::Participants),
                "{left}/{right}",
            );
        }
    }
    Ok(())
}

#[test]
fn each_participant_must_be_answered_before_any_bridge_handoff() -> Result<(), String> {
    let token = TransferToken::new(50);
    for role in 0..4 {
        let calls = participants([11, 12, 13, 14])?;
        let generation = calls[role].generation();
        for rejected in [
            Call::outgoing(generation),
            Call::outgoing(generation).apply(CallEvent::Ring).call(),
            calls[role].apply(CallEvent::Hangup).call(),
        ] {
            let mut invalid = calls;
            invalid[role] = rejected;
            assert_eq!(
                begin(invalid, 40, token),
                Err(TransferError::NotAnswered),
                "{role}/{:?}",
                rejected.state(),
            );
        }
    }
    Ok(())
}

#[test]
fn handoff_requires_two_distinct_current_bridges_and_a_fresh_destination() -> Result<(), String> {
    let token = TransferToken::new(50);
    let calls = participants([11, 12, 13, 14])?;
    for role in 0..4 {
        let current = calls[role].bridge().ok_or("Missing fixture bridge")?;
        for replacement in [None, Some(32)] {
            let mut invalid = calls;
            invalid[role] = invalid[role].leave(current).map_err(failure)?;
            if let Some(bridge) = replacement {
                invalid[role] = invalid[role].join(bridge).map_err(failure)?;
            }
            assert_eq!(begin(invalid, 40, token), Err(TransferError::Bridge));
        }
    }
    let one_bridge = [
        calls[0],
        calls[1],
        participant(13, 30)?,
        participant(14, 30)?,
    ];
    assert_eq!(begin(one_bridge, 40, token), Err(TransferError::Bridge));
    for reused in [30, 31] {
        assert_eq!(begin(calls, reused, token), Err(TransferError::Bridge));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Observation {
    Succeeded,
    Failed,
    Cancelled,
    TransferorEnded,
    ConsultationEnded,
    TransfereeEnded,
    TargetEnded,
    UnrelatedEnded,
    StaleResult,
    StaleParticipant,
}

#[test]
fn short_observation_sequences_release_one_monitor_at_the_first_current_terminal(
) -> Result<(), String> {
    let observations = [
        Observation::Succeeded,
        Observation::Failed,
        Observation::Cancelled,
        Observation::TransferorEnded,
        Observation::ConsultationEnded,
        Observation::TransfereeEnded,
        Observation::TargetEnded,
        Observation::UnrelatedEnded,
        Observation::StaleResult,
        Observation::StaleParticipant,
    ];
    for generation in [0_u64, 1, u64::MAX] {
        let generations = [
            generation,
            generation.wrapping_add(1),
            generation.wrapping_add(2),
            generation.wrapping_add(3),
        ];
        let token = TransferToken::new(generation);
        let other = TransferToken::new(generation.wrapping_add(1));
        for mut sequence in 0..observations.len().pow(3) {
            let mut transfer = begin(participants(generations)?, 40, token)
                .map_err(failure)?
                .transfer;
            let mut finished = None;
            let mut releases = 0;
            for _ in 0..3 {
                let observation = observations[sequence % observations.len()];
                sequence /= observations.len();
                let expected = match observation {
                    Observation::Succeeded => Some(TransferOutcome::Succeeded),
                    Observation::Failed => Some(TransferOutcome::Failed),
                    Observation::Cancelled
                    | Observation::TransfereeEnded
                    | Observation::TargetEnded => Some(TransferOutcome::Cancelled),
                    Observation::TransferorEnded
                    | Observation::ConsultationEnded
                    | Observation::UnrelatedEnded
                    | Observation::StaleResult
                    | Observation::StaleParticipant => None,
                };
                let decision = match observation {
                    Observation::Succeeded => transfer.finish(token, TransferOutcome::Succeeded),
                    Observation::Failed => transfer.finish(token, TransferOutcome::Failed),
                    Observation::Cancelled => transfer.finish(token, TransferOutcome::Cancelled),
                    Observation::TransferorEnded => {
                        transfer.participant_ended(token, generations[0])
                    }
                    Observation::ConsultationEnded => {
                        transfer.participant_ended(token, generations[2])
                    }
                    Observation::TransfereeEnded => {
                        transfer.participant_ended(token, generations[1])
                    }
                    Observation::TargetEnded => transfer.participant_ended(token, generations[3]),
                    Observation::UnrelatedEnded => {
                        transfer.participant_ended(token, generation.wrapping_add(4))
                    }
                    Observation::StaleResult => transfer.finish(other, TransferOutcome::Succeeded),
                    Observation::StaleParticipant => {
                        transfer.participant_ended(other, generations[1])
                    }
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
                        releases += 1;
                    }
                    (None, None) | (Some(_), _) => assert_eq!(decision, TransferDecision::Ignore),
                }
                assert_eq!(
                    transfer.state(),
                    finished.map_or(TransferState::Proceeding, TransferState::Finished),
                );
            }
            assert_eq!(releases, usize::from(finished.is_some()));
        }
    }
    Ok(())
}
