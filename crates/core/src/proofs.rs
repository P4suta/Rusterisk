use crate::{
    AttendedStart, AttendedTransfer, AudioGate, Call, CallEvent, CallPairDecision, CallPairError,
    CallPairState, CallState, ConnectionClaim, CoreError, MediaConnection, MediaState,
    MediaTaskError, MediaTaskKind, MediaTaskState, MediaTasks, MuteDirection, OwnedCallPair,
    PeerMediaDirection, SettingsError, SettingsLifetimes, StasisApplication, StasisMembership,
    Transfer, TransferDecision, TransferError, TransferOutcome, TransferState, TransferToken,
    Transition,
};

#[derive(Clone, Copy, kani::Arbitrary)]
enum Input {
    Ring,
    AdmitEarly,
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
        Input::AdmitEarly => {
            let admitted = call.admit_early_media();
            assert_eq!(admitted.is_ok(), call.state() == CallState::Ringing);
            match admitted {
                Ok(early) => {
                    assert_eq!(early.state(), CallState::Ringing);
                    assert_eq!(early.media(), MediaState::Early);
                    assert_eq!(early.generation(), call.generation());
                    assert_eq!(early.media_connection(), call.media_connection());
                    assert_eq!(early.connection_claim(), call.connection_claim());
                    assert_eq!(early.mute, call.mute);
                    assert_eq!(early.peer_direction(), call.peer_direction());
                    assert_eq!(early.bridge(), call.bridge());
                    assert_eq!(early.stasis(), call.stasis());
                    assert_eq!(early.audio_gate(), AudioGate::Closed);
                    assert_eq!(
                        early.generated_audio_gate(),
                        call.peer_direction.restrict(call.mute)
                    );
                    assert_eq!(early.admit_early_media(), Ok(early));
                    let answered = early.apply(CallEvent::Answer).call();
                    assert_eq!(
                        answered,
                        Call {
                            state: CallState::Up,
                            media: MediaState::Active,
                            ..early
                        }
                    );
                }
                Err(error) => assert_eq!(
                    error,
                    if call.state() == CallState::Ended {
                        CoreError::Ended
                    } else {
                        CoreError::NotRinging
                    }
                ),
            }
            admitted
        }
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
            | CoreError::NotRinging
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
    let media_before = call.media();
    call = call.apply(kani::any::<Terminal>().event()).call();
    assert_eq!(call.generation(), generation);
    assert_eq!(call.state(), CallState::Ended);
    assert_eq!(call.media(), MediaState::Released);
    assert_eq!(call.media_connection(), MediaConnection::Released);
    assert_eq!(call.connection_claim(), ConnectionClaim::Released);
    assert_eq!(call.audio_gate(), AudioGate::Closed);
    assert_eq!(call.generated_audio_gate(), AudioGate::Closed);
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
    kani::cover!(media_before == MediaState::Early);
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
                assert_eq!(after.generated_audio_gate(), before.generated_audio_gate());
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
            1 => MediaState::Early,
            2 => MediaState::Active,
            3 => MediaState::Held,
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
#[kani::unwind(17)]
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
            assert_eq!(start.pair.bridge(), bridge);
            assert_eq!(
                start.pair.generations(),
                [transferee.generation(), target.generation()]
            );
            assert_eq!(start.pair.state(), CallPairState::Linked);
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
#[kani::unwind(17)]
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

#[kani::proof]
#[kani::unwind(17)]
fn owned_pair_departures_stay_with_the_current_bridge_and_generations() {
    let left = attended_arbitrary_call();
    let right = attended_arbitrary_call();
    let distinct = left.generation() != right.generation();
    let answered = left.state() == CallState::Up && right.state() == CallState::Up;
    let bridged = left.bridge().is_some() && left.bridge() == right.bridge();
    let result = OwnedCallPair::new(left, right);
    let mut ended_peer = false;
    let mut released_bridge = false;
    let mut ignored = false;
    match result {
        Ok(mut pair) => {
            assert!(distinct && answered && bridged);
            assert_eq!(Some(pair.bridge()), left.bridge());
            assert_eq!(pair.generations(), [left.generation(), right.generation()]);
            assert_eq!(pair.state(), CallPairState::Linked);
            let before = pair;
            let observed_bridge = kani::any();
            let generation = kani::any();
            let current_peer = if kani::any::<bool>() {
                Some(attended_arbitrary_call())
            } else {
                None
            };
            let current = observed_bridge == pair.bridge()
                && (generation == left.generation() || generation == right.generation());
            let peer = if generation == left.generation() {
                right.generation()
            } else {
                left.generation()
            };
            let can_end = current_peer.is_some_and(|call| {
                call.generation() == peer
                    && call.bridge() == left.bridge()
                    && call.state() == CallState::Up
            });
            let decision = pair.participant_ended(observed_bridge, generation, current_peer);
            if current && can_end {
                assert_eq!(
                    decision,
                    CallPairDecision::EndPeer {
                        peer,
                        release: before.bridge()
                    }
                );
                assert_eq!(pair.state(), CallPairState::Released);
                ended_peer = true;
            } else if current {
                assert_eq!(
                    decision,
                    CallPairDecision::ReleaseBridge {
                        release: before.bridge()
                    }
                );
                assert_eq!(pair.state(), CallPairState::Released);
                released_bridge = true;
            } else {
                assert_eq!(decision, CallPairDecision::Ignore);
                assert_eq!(pair, before);
                ignored = true;
            }
            assert_eq!(pair.bridge(), before.bridge());
            assert_eq!(pair.generations(), before.generations());
        }
        Err(CallPairError::Participants) => assert!(!distinct),
        Err(CallPairError::NotAnswered) => assert!(distinct && !answered),
        Err(CallPairError::Bridge) => assert!(distinct && answered && !bridged),
    }
    kani::cover!(result.is_ok());
    kani::cover!(result == Err(CallPairError::Participants));
    kani::cover!(result == Err(CallPairError::NotAnswered));
    kani::cover!(result == Err(CallPairError::Bridge));
    kani::cover!(ended_peer);
    kani::cover!(released_bridge);
    kani::cover!(ignored);
}

#[kani::proof]
#[kani::unwind(17)]
fn owned_pair_release_is_absorbing() {
    let generations: [u64; 2] = kani::any();
    let bridge = kani::any();
    let call = |generation| Call {
        state: CallState::Up,
        bridge: Some(bridge),
        ..Call::outgoing(generation)
    };
    let result = OwnedCallPair::new(call(generations[0]), call(generations[1]));
    let Ok(mut pair) = result else {
        assert_eq!(result, Err(CallPairError::Participants));
        return;
    };
    let mut released = false;
    let mut releases = 0;
    let mut ended_peer = false;
    let mut retired = false;
    let mut moved_peer = false;
    for _ in 0..4 {
        let observed_bridge = kani::any();
        let generation = kani::any();
        let retire: bool = kani::any();
        let current_peer = if kani::any::<bool>() {
            Some(attended_arbitrary_call())
        } else {
            None
        };
        let member = generation == generations[0] || generation == generations[1];
        let current = !released && observed_bridge == bridge && (retire || member);
        let peer = if generation == generations[0] {
            generations[1]
        } else {
            generations[0]
        };
        let can_end = current_peer.is_some_and(|call| {
            call.generation() == peer
                && call.bridge() == Some(bridge)
                && call.state() == CallState::Up
        });
        let decision = if retire {
            pair.retire(observed_bridge)
        } else {
            pair.participant_ended(observed_bridge, generation, current_peer)
        };
        if current {
            if !retire && can_end {
                assert_eq!(
                    decision,
                    CallPairDecision::EndPeer {
                        peer,
                        release: bridge
                    }
                );
                ended_peer = true;
            } else {
                assert_eq!(
                    decision,
                    CallPairDecision::ReleaseBridge { release: bridge }
                );
            }
            retired |= retire;
            moved_peer |= !retire
                && current_peer.is_some_and(|call| {
                    call.generation() == peer
                        && call.bridge() != Some(bridge)
                        && call.state() == CallState::Up
                });
            released = true;
            releases += 1;
        } else {
            assert_eq!(decision, CallPairDecision::Ignore);
        }
        assert_eq!(
            pair.state(),
            if released {
                CallPairState::Released
            } else {
                CallPairState::Linked
            }
        );
        assert_eq!(pair.bridge(), bridge);
        assert_eq!(pair.generations(), generations);
        assert!(releases <= 1);
    }
    assert_eq!(releases, usize::from(released));
    kani::cover!(ended_peer);
    kani::cover!(retired);
    kani::cover!(moved_peer);
    kani::cover!(releases == 0);
}

#[derive(Debug, PartialEq, Eq)]
struct ProofFailure;

fn reference_lifetime(owner: u128, reverse: bool) -> Result<(), ProofFailure> {
    let mut settings = SettingsLifetimes::new(owner);
    let initial = settings.current();
    let slot = settings.slot(initial).map_err(|_| ProofFailure)?;
    let first = settings.retain(initial).map_err(|_| ProofFailure)?;
    let second = settings.retain(initial).map_err(|_| ProofFailure)?;
    let candidate = settings.prepare().map_err(|_| ProofFailure)?;
    let publication = settings.commit(candidate).map_err(|_| ProofFailure)?;
    assert_ne!(publication.generation(), initial);
    assert_eq!(publication.retired_slot(), None);
    let (first, second) = if reverse {
        (second, first)
    } else {
        (first, second)
    };
    assert_eq!(settings.release(first).map_err(|_| ProofFailure)?, None);
    assert_eq!(settings.slot(initial).map_err(|_| ProofFailure)?, slot);
    assert_eq!(
        settings.release(second).map_err(|_| ProofFailure)?,
        Some(slot)
    );
    assert_eq!(settings.current(), publication.generation());
    Ok(())
}

#[kani::proof]
#[kani::unwind(3)]
fn settings_references_preserve_their_generation_until_release() {
    let reverse: bool = kani::any();
    let result = reference_lifetime(kani::any(), reverse);
    assert_eq!(result, Ok(()));
    kani::cover!(result.is_ok() && reverse);
    kani::cover!(result.is_ok() && !reverse);
}

fn candidate_scope(first_owner: u128, second_owner: u128) -> Result<(), ProofFailure> {
    if first_owner == second_owner {
        return Ok(());
    }
    let mut first = SettingsLifetimes::new(first_owner);
    let mut second = SettingsLifetimes::new(second_owner);
    let foreign = first.prepare().map_err(|_| ProofFailure)?;
    let cancelled = second.prepare().map_err(|_| ProofFailure)?;
    second.cancel(cancelled).map_err(|_| ProofFailure)?;
    let selected = second.prepare().map_err(|_| ProofFailure)?;
    let current = second.current();
    assert!(foreign.follows(first.current()));
    assert!(selected.follows(current));
    assert!(!foreign.follows(current));
    assert!(!current.matches_candidate(selected));
    for rejected in [foreign, cancelled] {
        assert!(second.cancel(rejected).is_err());
        assert!(second.commit(rejected).is_err());
        assert_eq!(second.current(), current);
    }
    let published = second.commit(selected).map_err(|_| ProofFailure)?;
    assert!(published.generation().matches_candidate(selected));
    assert!(!published.generation().matches_candidate(cancelled));
    assert!(!published.generation().matches_candidate(foreign));
    assert!(!selected.follows(published.generation()));
    assert_eq!(second.current(), published.generation());
    first.cancel(foreign).map_err(|_| ProofFailure)?;
    Ok(())
}

#[kani::proof]
#[kani::unwind(3)]
fn settings_candidates_cannot_cross_domains_or_replace_later_candidates() {
    let first: u128 = kani::any();
    let second: u128 = kani::any();
    let result = candidate_scope(first, second);
    assert_eq!(result, Ok(()));
    kani::cover!(first != second && result.is_ok());
}

fn foreign_reference(first_owner: u128, second_owner: u128) -> Result<(), ProofFailure> {
    if first_owner == second_owner {
        return Ok(());
    }
    let mut first = SettingsLifetimes::new(first_owner);
    let mut second = SettingsLifetimes::new(second_owner);
    let foreign = first.retain(first.current()).map_err(|_| ProofFailure)?;
    let local = second.retain(second.current()).map_err(|_| ProofFailure)?;
    let first_generation = foreign.generation();
    let second_generation = local.generation();
    let second_slot = second.slot(second_generation).map_err(|_| ProofFailure)?;
    let recovered = match second.release(foreign) {
        Err(error) => {
            assert!(matches!(&error, SettingsError::ForeignReference(_)));
            error.into_reference().ok_or(ProofFailure)?
        }
        Ok(_) => return Err(ProofFailure),
    };
    assert_eq!(recovered.generation(), first_generation);
    assert_eq!(first.release(recovered).map_err(|_| ProofFailure)?, None);
    let candidate = second.prepare().map_err(|_| ProofFailure)?;
    let published = second.commit(candidate).map_err(|_| ProofFailure)?;
    assert_eq!(published.retired_slot(), None);
    assert_eq!(
        second.slot(second_generation).map_err(|_| ProofFailure)?,
        second_slot
    );
    assert_eq!(
        second.release(local).map_err(|_| ProofFailure)?,
        Some(second_slot)
    );
    assert_eq!(second.current(), published.generation());
    Ok(())
}

#[kani::proof]
#[kani::unwind(3)]
fn settings_foreign_references_leave_both_domains_unchanged() {
    let first: u128 = kani::any();
    let second: u128 = kani::any();
    let result = foreign_reference(first, second);
    assert_eq!(result, Ok(()));
    kani::cover!(first != second && result.is_ok());
}

fn reference_count_boundary(owner: u128, references: u64) -> Result<(), SettingsError> {
    let (mut settings, generation) = SettingsLifetimes::reference_count_state(owner, references);
    let retained = settings.retain(generation);
    if let Some(expected) = references.checked_add(1) {
        let reference = retained?;
        assert_eq!(reference.generation(), generation);
        assert_eq!(settings.current_reference_count(), Some(expected));
    } else {
        assert!(matches!(retained, Err(SettingsError::ReferenceCapacity)));
        assert_eq!(settings.current_reference_count(), Some(references));
    }
    assert_eq!(settings.current(), generation);
    Ok(())
}

#[kani::proof]
#[kani::unwind(2)]
fn settings_reference_count_boundaries_preserve_publication() {
    let references: u64 = kani::any();
    let result = reference_count_boundary(kani::any(), references);
    assert_eq!(result, Ok(()));
    kani::cover!(result.is_ok() && references == u64::MAX);
    kani::cover!(result.is_ok() && references < u64::MAX);
}
