use crate::{
    AttendedStart, AttendedTransfer, AudioGate, Call, CallEvent, CallState, ConnectionClaim,
    CoreError, MediaConnection, MediaState, MediaTaskError, MediaTaskKind, MediaTaskState,
    MediaTasks, MuteDirection, PeerMediaDirection, StasisApplication, StasisMembership, Transfer,
    TransferDecision, TransferError, TransferOutcome, TransferState, TransferToken, Transition,
};

#[derive(Clone, Copy, kani::Arbitrary)]
enum Input {
    Ring,
    Answer,
    Cancel,
    Hangup,
    Timeout,
    Claim,
    Connect,
    Hold,
    Resume,
    Mute,
    Unmute,
    Join,
    Leave,
    ReserveStasis,
    EnterStasis,
    LeaveStasis,
    PeerInactive,
}

#[derive(Clone, Copy, kani::Arbitrary)]
enum Terminal {
    Cancel,
    Hangup,
    Timeout,
}

impl Terminal {
    fn event(self) -> CallEvent {
        match self {
            Self::Cancel => CallEvent::Cancel,
            Self::Hangup => CallEvent::Hangup,
            Self::Timeout => CallEvent::Timeout,
        }
    }
}

fn input(call: Call, operation: Input) -> Call {
    let app = StasisApplication::new(1);
    let result = match operation {
        Input::Ring => Ok(call.apply(CallEvent::Ring).call()),
        Input::Answer => Ok(call.apply(CallEvent::Answer).call()),
        Input::Cancel => Ok(call.apply(CallEvent::Cancel).call()),
        Input::Hangup => Ok(call.apply(CallEvent::Hangup).call()),
        Input::Timeout => Ok(call.apply(CallEvent::Timeout).call()),
        Input::Claim => call.claim_connection(),
        Input::Connect => Ok(call.connect_media().call()),
        Input::Hold => call.hold(),
        Input::Resume => call.resume(),
        Input::Mute => call.mute(MuteDirection::Both),
        Input::Unmute => call.unmute(MuteDirection::Both),
        Input::Join => call.join(1),
        Input::Leave => call.leave(1),
        Input::ReserveStasis => call.reserve_stasis(app),
        Input::EnterStasis => call.enter_stasis(app),
        Input::LeaveStasis => call.leave_stasis(app),
        Input::PeerInactive => call.with_peer_direction(PeerMediaDirection::Inactive),
    };
    match result {
        Ok(updated) => updated,
        Err(
            CoreError::ConnectionClaimed
            | CoreError::Ended
            | CoreError::NotAnswered
            | CoreError::NotHeld
            | CoreError::AlreadyBridged
            | CoreError::WrongBridge
            | CoreError::StasisConflict
            | CoreError::WrongStasisApplication
            | CoreError::NotInStasis,
        ) => call,
    }
}

#[kani::proof]
#[kani::unwind(12)]
fn terminal_release_is_absorbing() {
    let generation: u64 = kani::any();
    let mut call = Call::outgoing(generation);
    for operation in kani::any::<[Input; 4]>() {
        call = input(call, operation);
    }
    let before = call.state();
    call = call.apply(kani::any::<Terminal>().event()).call();
    assert_eq!(call.generation(), generation);
    assert_eq!(call.state(), CallState::Ended);
    assert_eq!(call.media(), MediaState::Released);
    assert_eq!(call.media_connection(), MediaConnection::Released);
    assert_eq!(call.connection_claim(), ConnectionClaim::Released);
    assert_eq!(call.audio_gate(), AudioGate::Closed);
    assert_eq!(call.bridge(), None);
    assert_eq!(call.stasis(), StasisMembership::Released);
    let ended = call;
    for operation in kani::any::<[Input; 4]>() {
        call = input(call, operation);
        assert_eq!(call, ended);
    }
    assert_eq!(call.connect_media().transition(), Transition::Ignore);
    kani::cover!(before == CallState::Down);
    kani::cover!(before == CallState::Ringing);
    kani::cover!(before == CallState::Up);
    kani::cover!(before == CallState::Ended);
}

#[kani::proof]
#[kani::unwind(12)]
fn connection_claims_are_exclusive() {
    let generation: u64 = kani::any();
    let answered: bool = kani::any();
    let call = if answered {
        Call::outgoing(generation).apply(CallEvent::Answer).call()
    } else {
        Call::outgoing(generation)
    };
    assert_eq!(call.connection_claim(), ConnectionClaim::Available);
    let Ok(claimed) = call.claim_connection() else {
        assert!(false, "first connection claim failed");
        return;
    };
    assert_eq!(claimed.connection_claim(), ConnectionClaim::Claimed);
    assert_eq!(claimed.generation(), generation);
    assert_eq!(
        claimed.claim_connection(),
        Err(CoreError::ConnectionClaimed)
    );
    let ended = claimed.apply(kani::any::<Terminal>().event()).call();
    assert_eq!(ended.connection_claim(), ConnectionClaim::Released);
    assert_eq!(ended.claim_connection(), Err(CoreError::Ended));
    kani::cover!(answered);
    kani::cover!(!answered);
}

#[kani::proof]
#[kani::unwind(12)]
fn stasis_tracks_the_current_app() {
    let generation: u64 = kani::any();
    let app_id: u64 = kani::any();
    let other_app_id: u64 = kani::any();
    if app_id == other_app_id {
        return;
    }
    let app = StasisApplication::new(app_id);
    let other = StasisApplication::new(other_app_id);
    let outgoing: bool = kani::any();
    let call = if outgoing {
        let Ok(pending) = Call::outgoing(generation).reserve_stasis(app) else {
            assert!(false, "Stasis reservation failed");
            return;
        };
        assert_eq!(pending.enter_stasis(app), Err(CoreError::NotAnswered));
        pending.apply(CallEvent::Answer).call()
    } else {
        Call::incoming(generation)
    };
    let Ok(entered) = call.enter_stasis(app) else {
        assert!(false, "Stasis entry failed");
        return;
    };
    assert_eq!(entered.enter_stasis(other), Err(CoreError::StasisConflict));
    assert_eq!(
        entered.move_stasis(other, app),
        Err(CoreError::WrongStasisApplication)
    );
    let Ok(moved) = entered.move_stasis(app, other) else {
        assert!(false, "Stasis move failed");
        return;
    };
    assert_eq!(moved.stasis(), StasisMembership::Entered(other));
    assert_eq!(
        moved.leave_stasis(app),
        Err(CoreError::WrongStasisApplication)
    );
    let Ok(left) = moved.leave_stasis(other) else {
        assert!(false, "Stasis exit failed");
        return;
    };
    assert_eq!(left.stasis(), StasisMembership::Outside);
    let ended = moved.apply(kani::any::<Terminal>().event()).call();
    assert_eq!(ended.stasis(), StasisMembership::Released);
    assert_eq!(ended.enter_stasis(app), Err(CoreError::Ended));
    assert_eq!(ended.move_stasis(other, app), Err(CoreError::Ended));
    assert_eq!(ended.leave_stasis(other), Err(CoreError::Ended));
    kani::cover!(outgoing);
    kani::cover!(!outgoing);
}

#[kani::proof]
#[kani::unwind(12)]
fn bridge_membership_is_exclusive() {
    let generation: u64 = kani::any();
    let bridge: u64 = kani::any();
    let other: u64 = kani::any();
    if bridge == other {
        return;
    }
    let call = Call::outgoing(generation);
    assert_eq!(call.join(bridge), Err(CoreError::NotAnswered));
    let Ok(joined) = call.apply(CallEvent::Answer).call().join(bridge) else {
        assert!(false, "bridge join failed");
        return;
    };
    assert_eq!(joined.bridge(), Some(bridge));
    assert_eq!(joined.join(other), Err(CoreError::AlreadyBridged));
    assert_eq!(joined.leave(other), Err(CoreError::WrongBridge));
    let Ok(left) = joined.leave(bridge) else {
        assert!(false, "bridge exit failed");
        return;
    };
    assert_eq!(left.bridge(), None);
    let Ok(rejoined) = left.join(other) else {
        assert!(false, "bridge replacement failed");
        return;
    };
    let ended = rejoined.apply(kani::any::<Terminal>().event()).call();
    assert_eq!(ended.bridge(), None);
    assert_eq!(ended.join(bridge), Err(CoreError::Ended));
    assert_eq!(ended.generation(), generation);
    kani::cover!(bridge == u64::MAX);
}

#[kani::proof]
#[kani::unwind(12)]
fn cancellation_releases_every_media_slot() {
    let generation: u64 = kani::any();
    let mut tasks = MediaTasks::new(generation);
    let mut tokens = [None; 8];
    for token in &mut tokens {
        let Ok(reserved) = tasks.reserve(MediaTaskKind::Playback) else {
            assert!(false, "media slot reservation failed");
            return;
        };
        *token = Some(reserved);
    }
    assert!(tasks.reserve(MediaTaskKind::Playback) == Err(MediaTaskError::Capacity));
    let released = tasks.cancel();
    assert!(released == tokens);
    assert!(tasks.cancel() == [None; 8]);
    for token in tokens {
        let Some(token) = token else {
            assert!(false, "reserved media token missing");
            return;
        };
        assert!(tasks.state(token).is_none());
        assert!(tasks.start(token) == Err(MediaTaskError::Ended));
        assert!(tasks.pause(token) == Err(MediaTaskError::Ended));
        assert!(tasks.resume(token) == Err(MediaTaskError::Ended));
        assert!(!tasks.finish(token));
    }
    assert!(tasks.reserve(MediaTaskKind::Playback) == Err(MediaTaskError::Ended));
    assert!(tasks.reserve(MediaTaskKind::Recording) == Err(MediaTaskError::Ended));
    kani::cover!(generation == u64::MAX);
}

#[kani::proof]
#[kani::unwind(12)]
fn media_tokens_do_not_cross_generations() {
    let generation: u64 = kani::any();
    let other_generation: u64 = kani::any();
    if generation == other_generation {
        return;
    }
    let recording: bool = kani::any();
    let kind = if recording {
        MediaTaskKind::Recording
    } else {
        MediaTaskKind::Playback
    };
    let mut prior = MediaTasks::new(generation);
    let mut current = MediaTasks::new(other_generation);
    let Ok(old) = prior.reserve(kind) else {
        assert!(false, "prior media reservation failed");
        return;
    };
    let Ok(new) = current.reserve(kind) else {
        assert!(false, "current media reservation failed");
        return;
    };
    assert!(old != new);
    assert!(current.state(old).is_none());
    assert!(current.start(old) == Err(MediaTaskError::Unknown));
    assert!(!current.finish(old));
    assert!(current.state(new) == Some(MediaTaskState::Queued));
    prior.cancel();
    assert!(!current.finish(old));
    assert!(current.state(new) == Some(MediaTaskState::Queued));
    assert!(current.finish(new));
    assert!(!current.finish(new));
    kani::cover!(recording);
    kani::cover!(!recording);
}

#[derive(Clone, Copy, kani::Arbitrary)]
enum TransferObservation {
    Succeeded,
    Failed,
    Cancelled,
    TransferorEnded,
    TransfereeEnded,
    ObservedResult,
}

#[derive(Clone, Copy, kani::Arbitrary)]
enum TransferCallState {
    Down,
    Ringing,
    Up,
    Ended,
}

fn transfer_proof_call(
    generation: u64,
    state: TransferCallState,
    bridge: Option<u64>,
    flags: [bool; 5],
) -> Result<Call, CoreError> {
    let mut call = Call::outgoing(generation);
    call = match state {
        TransferCallState::Down => call,
        TransferCallState::Ringing => call.apply(CallEvent::Ring).call(),
        TransferCallState::Up => call.apply(CallEvent::Answer).call(),
        TransferCallState::Ended => call.apply(CallEvent::Hangup).call(),
    };
    if call.state() == CallState::Up {
        if flags[0] {
            call = call.connect_media().call();
        }
        if flags[1] {
            call = call.hold()?;
        }
        if flags[2] {
            call = call.mute(MuteDirection::Both)?;
        }
        if flags[3] {
            call = call.enter_stasis(StasisApplication::new(generation))?;
        }
        if flags[4] {
            call = call.claim_connection()?;
        }
        if let Some(bridge) = bridge {
            call = call.join(bridge)?;
        }
    }
    Ok(call)
}

#[kani::proof]
#[kani::unwind(12)]
fn transfer_completion_is_absorbing() {
    let generation: u64 = kani::any();
    let other_generation: u64 = kani::any();
    let token = TransferToken::new(kani::any());
    let other = TransferToken::new(kani::any());
    let Ok(left) = transfer_proof_call(generation, TransferCallState::Up, Some(1), [false; 5])
    else {
        assert!(false, "transferor construction failed");
        return;
    };
    let Ok(right) =
        transfer_proof_call(other_generation, TransferCallState::Up, Some(1), [false; 5])
    else {
        assert!(false, "transferee construction failed");
        return;
    };
    let started = Transfer::begin(token, left, right);
    kani::cover!(generation == other_generation);
    if generation == other_generation {
        assert_eq!(started, Err(TransferError::Participants));
        return;
    }
    let Ok(start) = started else {
        assert!(false, "valid transfer was rejected");
        return;
    };
    let mut transfer = start.transfer;
    let mut released = 0;
    let mut terminal = None;
    for observation in kani::any::<[TransferObservation; 4]>() {
        let decision = match observation {
            TransferObservation::Succeeded => transfer.finish(token, TransferOutcome::Succeeded),
            TransferObservation::Failed => transfer.finish(token, TransferOutcome::Failed),
            TransferObservation::Cancelled => transfer.finish(token, TransferOutcome::Cancelled),
            TransferObservation::TransferorEnded => transfer.participant_ended(token, generation),
            TransferObservation::TransfereeEnded => {
                transfer.participant_ended(token, other_generation)
            }
            TransferObservation::ObservedResult => {
                transfer.finish(other, TransferOutcome::Succeeded)
            }
        };
        match decision {
            TransferDecision::Ignore => {}
            TransferDecision::Finished { outcome, release } => {
                assert_eq!(release, token);
                assert!(terminal.is_none());
                released += 1;
                terminal = Some(outcome);
            }
        }
        assert!(released <= 1);
        assert_eq!(
            transfer.state(),
            terminal.map_or(TransferState::Proceeding, TransferState::Finished)
        );
    }
    kani::cover!(released == 0);
    kani::cover!(token == other);
    kani::cover!(token != other);
    kani::cover!(terminal == Some(TransferOutcome::Succeeded));
    kani::cover!(terminal == Some(TransferOutcome::Failed));
    kani::cover!(terminal == Some(TransferOutcome::Cancelled));
}

#[kani::proof]
#[kani::unwind(12)]
fn transfer_observations_stay_in_their_generation() {
    let generation: u64 = kani::any();
    let other_generation: u64 = kani::any();
    let token = TransferToken::new(kani::any());
    let other = TransferToken::new(kani::any());
    let Ok(left) = transfer_proof_call(generation, TransferCallState::Up, Some(1), [false; 5])
    else {
        assert!(false, "transferor construction failed");
        return;
    };
    let Ok(right) =
        transfer_proof_call(other_generation, TransferCallState::Up, Some(1), [false; 5])
    else {
        assert!(false, "transferee construction failed");
        return;
    };
    let started = Transfer::begin(token, left, right);
    kani::cover!(generation == other_generation);
    if generation == other_generation {
        assert_eq!(started, Err(TransferError::Participants));
        return;
    }
    let Ok(start) = started else {
        assert!(false, "valid transfer was rejected");
        return;
    };
    let mut transfer = start.transfer;
    let finished: bool = kani::any();
    if finished {
        assert_eq!(
            transfer.finish(token, TransferOutcome::Succeeded),
            TransferDecision::Finished {
                outcome: TransferOutcome::Succeeded,
                release: token
            }
        );
    }
    kani::cover!(token == other && finished);
    kani::cover!(token == other && !finished);
    kani::cover!(token != other && finished);
    kani::cover!(token != other && !finished);
    if token == other {
        let decision = transfer.finish(other, TransferOutcome::Succeeded);
        let expected = if finished {
            TransferDecision::Ignore
        } else {
            TransferDecision::Finished {
                outcome: TransferOutcome::Succeeded,
                release: token,
            }
        };
        assert_eq!(decision, expected);
        assert_eq!(
            transfer.state(),
            TransferState::Finished(TransferOutcome::Succeeded)
        );
        assert_eq!(
            transfer.participant_ended(other, other_generation),
            TransferDecision::Ignore
        );
        return;
    }
    let before = transfer;
    assert_eq!(
        transfer.finish(other, TransferOutcome::Succeeded),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(other, other_generation),
        TransferDecision::Ignore
    );
    assert_eq!(
        transfer.participant_ended(token, generation),
        TransferDecision::Ignore
    );
    assert_eq!(transfer, before);
    kani::cover!(finished);
    kani::cover!(!finished);
}

#[kani::proof]
#[kani::unwind(12)]
fn transfer_detachment_preserves_live_calls() {
    let token = TransferToken::new(kani::any());
    let Ok(left) = transfer_proof_call(kani::any(), kani::any(), kani::any(), kani::any()) else {
        assert!(false, "transferor construction failed");
        return;
    };
    let Ok(right) = transfer_proof_call(kani::any(), kani::any(), kani::any(), kani::any()) else {
        assert!(false, "transferee construction failed");
        return;
    };
    let valid = left.generation() != right.generation()
        && left.state() == CallState::Up
        && right.state() == CallState::Up
        && left.bridge().is_some()
        && left.bridge() == right.bridge();
    match Transfer::begin(token, left, right) {
        Ok(start) => {
            assert!(valid);
            assert_eq!(Some(start.release_bridge), left.bridge());
            for (before, after) in [(left, start.transferor), (right, start.transferee)] {
                assert_eq!(after.generation(), before.generation());
                assert_eq!(after.state(), CallState::Up);
                assert_eq!(after.bridge(), None);
                assert_eq!(after.media(), before.media());
                assert_eq!(after.media_connection(), before.media_connection());
                assert_eq!(after.connection_claim(), before.connection_claim());
                assert_eq!(after.audio_gate(), before.audio_gate());
                assert_eq!(after.peer_direction(), before.peer_direction());
                assert_eq!(after.stasis(), before.stasis());
            }
            assert_eq!(start.transfer.state(), TransferState::Proceeding);
        }
        Err(TransferError::Participants | TransferError::NotAnswered | TransferError::Bridge) => {
            assert!(!valid);
        }
    }
    kani::cover!(valid);
    kani::cover!(!valid);
}

fn attended_arbitrary_call() -> Call {
    let application = StasisApplication::new(kani::any());
    Call {
        generation: kani::any(),
        state: match kani::any::<u8>() {
            0 => CallState::Down,
            1 => CallState::Ringing,
            2 => CallState::Up,
            _ => CallState::Ended,
        },
        media: match kani::any::<u8>() {
            0 => MediaState::Waiting,
            1 => MediaState::Active,
            2 => MediaState::Held,
            _ => MediaState::Released,
        },
        connection: match kani::any::<u8>() {
            0 => MediaConnection::Pending,
            1 => MediaConnection::Connected,
            _ => MediaConnection::Released,
        },
        connection_claim: match kani::any::<u8>() {
            0 => ConnectionClaim::Available,
            1 => ConnectionClaim::Claimed,
            _ => ConnectionClaim::Released,
        },
        mute: match kani::any::<u8>() {
            0 => AudioGate::Open,
            1 => AudioGate::IncomingMuted,
            2 => AudioGate::OutgoingMuted,
            _ => AudioGate::Closed,
        },
        peer_direction: match kani::any::<u8>() {
            0 => PeerMediaDirection::SendReceive,
            1 => PeerMediaDirection::SendOnly,
            2 => PeerMediaDirection::ReceiveOnly,
            _ => PeerMediaDirection::Inactive,
        },
        bridge: kani::any(),
        stasis: match kani::any::<u8>() {
            0 => StasisMembership::Outside,
            1 => StasisMembership::Pending(application),
            2 => StasisMembership::Entered(application),
            _ => StasisMembership::Released,
        },
    }
}

#[kani::proof]
#[kani::unwind(12)]
fn attended_handoff_preserves_calls_and_bridge_ownership() {
    let transferor = attended_arbitrary_call();
    let transferee = attended_arbitrary_call();
    let consultation = attended_arbitrary_call();
    let target = attended_arbitrary_call();
    let bridge = kani::any();
    let token = TransferToken::new(kani::any());
    let distinct = transferor.generation() != transferee.generation()
        && transferor.generation() != consultation.generation()
        && transferor.generation() != target.generation()
        && transferee.generation() != consultation.generation()
        && transferee.generation() != target.generation()
        && consultation.generation() != target.generation();
    let answered = transferor.state() == CallState::Up
        && transferee.state() == CallState::Up
        && consultation.state() == CallState::Up
        && target.state() == CallState::Up;
    let bridges = transferor.bridge().is_some()
        && consultation.bridge().is_some()
        && transferor.bridge() == transferee.bridge()
        && consultation.bridge() == target.bridge()
        && transferor.bridge() != consultation.bridge()
        && Some(bridge) != transferor.bridge()
        && Some(bridge) != consultation.bridge();
    let result =
        AttendedTransfer::begin(token, bridge, transferor, transferee, consultation, target);
    match result {
        Ok(start) => {
            assert!(distinct && answered && bridges);
            assert_eq!(Some(start.release_bridges[0]), transferor.bridge());
            assert_eq!(Some(start.release_bridges[1]), consultation.bridge());
            assert_eq!(
                start.transferor,
                Call {
                    bridge: None,
                    ..transferor
                }
            );
            assert_eq!(
                start.consultation,
                Call {
                    bridge: None,
                    ..consultation
                }
            );
            assert_eq!(
                start.transferee,
                Call {
                    bridge: Some(bridge),
                    ..transferee
                }
            );
            assert_eq!(
                start.target,
                Call {
                    bridge: Some(bridge),
                    ..target
                }
            );
            assert_eq!(start.transfer.state(), TransferState::Proceeding);
        }
        Err(TransferError::Participants) => assert!(!distinct),
        Err(TransferError::NotAnswered) => assert!(distinct && !answered),
        Err(TransferError::Bridge) => assert!(distinct && answered && !bridges),
    }
    kani::cover!(result.is_ok());
    kani::cover!(result == Err(TransferError::Participants));
    kani::cover!(result == Err(TransferError::NotAnswered));
    kani::cover!(result == Err(TransferError::Bridge));
}

fn attended_proof_start(
    generations: [u64; 4],
    token: TransferToken,
) -> Result<AttendedStart, TransferError> {
    let call = |generation, bridge| Call {
        state: CallState::Up,
        bridge: Some(bridge),
        ..Call::outgoing(generation)
    };
    AttendedTransfer::begin(
        token,
        3,
        call(generations[0], 1),
        call(generations[1], 1),
        call(generations[2], 2),
        call(generations[3], 2),
    )
}

#[derive(Clone, Copy, kani::Arbitrary)]
enum AttendedOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl AttendedOutcome {
    const fn value(self) -> TransferOutcome {
        match self {
            Self::Succeeded => TransferOutcome::Succeeded,
            Self::Failed => TransferOutcome::Failed,
            Self::Cancelled => TransferOutcome::Cancelled,
        }
    }
}

#[kani::proof]
#[kani::unwind(12)]
fn attended_progress_stays_in_the_current_generation() {
    let generations: [u64; 4] = kani::any();
    let token = TransferToken::new(kani::any());
    let observed = TransferToken::new(kani::any());
    let result = attended_proof_start(generations, token);
    let Ok(start) = result else {
        assert_eq!(result, Err(TransferError::Participants));
        return;
    };
    let outcome = kani::any::<AttendedOutcome>().value();
    let mut terminal = start.transfer;
    let decision = terminal.finish(observed, outcome);
    if observed == token {
        assert_eq!(
            decision,
            TransferDecision::Finished {
                outcome,
                release: token
            }
        );
        assert_eq!(terminal.state(), TransferState::Finished(outcome));
    } else {
        assert_eq!(decision, TransferDecision::Ignore);
        assert_eq!(terminal, start.transfer);
    }
    let generation = kani::any();
    let mut participant = start.transfer;
    let decision = participant.participant_ended(observed, generation);
    if observed == token && (generation == generations[1] || generation == generations[3]) {
        assert_eq!(
            decision,
            TransferDecision::Finished {
                outcome: TransferOutcome::Cancelled,
                release: token
            },
        );
        assert_eq!(
            participant.state(),
            TransferState::Finished(TransferOutcome::Cancelled)
        );
    } else {
        assert_eq!(decision, TransferDecision::Ignore);
        assert_eq!(participant, start.transfer);
    }
    kani::cover!(observed == token);
    kani::cover!(observed != token);
    kani::cover!(observed == token && generation == generations[0]);
    kani::cover!(observed == token && generation == generations[1]);
    kani::cover!(observed == token && generation == generations[2]);
    kani::cover!(observed == token && generation == generations[3]);
}

#[derive(Clone, Copy, kani::Arbitrary)]
enum AttendedObservation {
    Result {
        token: u64,
        outcome: AttendedOutcome,
    },
    End {
        token: u64,
        generation: u64,
    },
}

#[kani::proof]
#[kani::unwind(12)]
fn attended_completion_is_absorbing() {
    let generations: [u64; 4] = kani::any();
    let token = TransferToken::new(kani::any());
    let result = attended_proof_start(generations, token);
    let Ok(start) = result else {
        assert_eq!(result, Err(TransferError::Participants));
        return;
    };
    let mut transfer = start.transfer;
    let mut finished = None;
    let mut releases = 0;
    let mut foreign = false;
    for observation in kani::any::<[AttendedObservation; 4]>() {
        let (observed, outcome, decision) = match observation {
            AttendedObservation::Result {
                token: observed,
                outcome,
            } => {
                let observed = TransferToken::new(observed);
                let outcome = outcome.value();
                (observed, Some(outcome), transfer.finish(observed, outcome))
            }
            AttendedObservation::End {
                token: observed,
                generation,
            } => {
                let observed = TransferToken::new(observed);
                let outcome = if generation == generations[1] || generation == generations[3] {
                    Some(TransferOutcome::Cancelled)
                } else {
                    None
                };
                (
                    observed,
                    outcome,
                    transfer.participant_ended(observed, generation),
                )
            }
        };
        foreign |= observed != token;
        if let Some(outcome) = outcome.filter(|_| finished.is_none() && observed == token) {
            assert_eq!(
                decision,
                TransferDecision::Finished {
                    outcome,
                    release: token
                }
            );
            finished = Some(outcome);
            releases += 1;
        } else {
            assert_eq!(decision, TransferDecision::Ignore);
        }
        assert_eq!(
            transfer.state(),
            finished.map_or(TransferState::Proceeding, TransferState::Finished)
        );
        assert!(releases <= 1);
    }
    assert_eq!(releases, usize::from(finished.is_some()));
    kani::cover!(finished == Some(TransferOutcome::Succeeded));
    kani::cover!(finished == Some(TransferOutcome::Failed));
    kani::cover!(finished == Some(TransferOutcome::Cancelled));
    kani::cover!(foreign);
}
