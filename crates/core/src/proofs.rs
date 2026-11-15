use crate::{
    AudioGate, Call, CallEvent, CallState, ConnectionClaim, CoreError, MediaConnection, MediaState,
    MediaTaskError, MediaTaskKind, MediaTaskState, MediaTasks, MuteDirection, PeerMediaDirection,
    StasisApplication, StasisMembership, Transition,
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
    let app = StasisApplication::new(app_id);
    let other = StasisApplication::new(app_id ^ 1);
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
    let other = bridge ^ 1;
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
