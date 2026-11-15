#![no_std]

mod dial_failure;
mod dial_fork;
mod dialplan;
mod dialplan_answer;
mod digit_admission;
mod digit_sequence;
mod digits;
mod media_tasks;
mod owned_progress;
mod peer_media;
#[cfg(kani)]
mod proofs;
mod recording_clock;
mod recording_gate;
mod recording_silence;
mod recording_stop_keys;
mod resource_subscriptions;
mod stasis;
pub use dial_failure::DialFailure;
pub use dial_fork::{DialFork, DialForkDecision, DialForkError, DialForkOutcome, DialForkState};
pub use dialplan::{
    DialplanContinuation, DialplanDispatch, DialplanError, DialplanExecution, DialplanInstruction,
    DialplanMachine, DialplanState, DialplanToken,
};
pub use dialplan_answer::{AnswerProgress, AnswerStatus};
pub use digit_admission::{DtmfCommand, DtmfOutbox, DtmfToken};
pub use digit_sequence::{DtmfError, DtmfSequence, DtmfTiming, DtmfTone};
pub use digits::DtmfDigit;
pub use media_tasks::{MediaTaskError, MediaTaskKind, MediaTaskState, MediaTaskToken, MediaTasks};
pub use owned_progress::{CallCompletion, CallMilestone, OwnedCallProgress};
pub use peer_media::PeerMediaDirection;
pub use recording_clock::RecordingClock;
pub use recording_gate::{RecordingControl, RecordingGate};
pub use recording_silence::RecordingSilence;
pub use recording_stop_keys::RecordingStopKey;
pub use resource_subscriptions::{
    ResourceKind, ResourceSource, ResourceSubscriptions, SubscriptionError,
};
pub use stasis::{StasisApplication, StasisMembership};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactPresence {
    Present,
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    Down,
    Ringing,
    Up,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaState {
    Waiting,
    Active,
    Held,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaConnection {
    Pending,
    Connected,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionClaim {
    Available,
    Claimed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuteDirection {
    Incoming,
    Outgoing,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioGate {
    Open,
    IncomingMuted,
    OutgoingMuted,
    Closed,
}

impl AudioGate {
    const fn mute(self, direction: MuteDirection) -> Self {
        match (self, direction) {
            (Self::Open | Self::IncomingMuted, MuteDirection::Incoming) => Self::IncomingMuted,
            (Self::OutgoingMuted | Self::Closed, MuteDirection::Incoming) => Self::Closed,
            (Self::Open | Self::OutgoingMuted, MuteDirection::Outgoing) => Self::OutgoingMuted,
            (Self::IncomingMuted | Self::Closed, MuteDirection::Outgoing) => Self::Closed,
            (
                Self::Open | Self::IncomingMuted | Self::OutgoingMuted | Self::Closed,
                MuteDirection::Both,
            ) => Self::Closed,
        }
    }

    const fn unmute(self, direction: MuteDirection) -> Self {
        match (self, direction) {
            (Self::Open | Self::IncomingMuted, MuteDirection::Incoming) => Self::Open,
            (Self::OutgoingMuted | Self::Closed, MuteDirection::Incoming) => Self::OutgoingMuted,
            (Self::Open | Self::OutgoingMuted, MuteDirection::Outgoing) => Self::Open,
            (Self::IncomingMuted | Self::Closed, MuteDirection::Outgoing) => Self::IncomingMuted,
            (
                Self::Open | Self::IncomingMuted | Self::OutgoingMuted | Self::Closed,
                MuteDirection::Both,
            ) => Self::Open,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallEvent {
    Ring,
    Answer,
    Cancel,
    Hangup,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Notify,
    Connect,
    Release,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    ConnectionClaimed,
    Ended,
    NotAnswered,
    NotHeld,
    AlreadyBridged,
    WrongBridge,
    StasisConflict,
    WrongStasisApplication,
    NotInStasis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    generation: u64,
    state: CallState,
    media: MediaState,
    connection: MediaConnection,
    connection_claim: ConnectionClaim,
    mute: AudioGate,
    peer_direction: PeerMediaDirection,
    bridge: Option<u64>,
    stasis: StasisMembership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    call: Call,
    transition: Transition,
}

impl Decision {
    pub const fn call(self) -> Call {
        self.call
    }

    pub const fn state(self) -> CallState {
        self.call.state
    }

    pub const fn transition(self) -> Transition {
        self.transition
    }
}

impl Call {
    pub const fn outgoing(generation: u64) -> Self {
        Self {
            generation,
            state: CallState::Down,
            media: MediaState::Waiting,
            connection: MediaConnection::Pending,
            connection_claim: ConnectionClaim::Available,
            mute: AudioGate::Open,
            peer_direction: PeerMediaDirection::SendReceive,
            bridge: None,
            stasis: StasisMembership::Outside,
        }
    }

    pub const fn incoming(generation: u64) -> Self {
        Self::outgoing(generation)
    }

    pub const fn state(self) -> CallState {
        self.state
    }

    pub const fn media(self) -> MediaState {
        self.media
    }

    pub const fn audio_gate(self) -> AudioGate {
        match self.media {
            MediaState::Active => self.peer_direction.restrict(self.mute),
            MediaState::Waiting | MediaState::Held | MediaState::Released => AudioGate::Closed,
        }
    }

    pub const fn peer_direction(self) -> PeerMediaDirection {
        self.peer_direction
    }

    pub fn with_peer_direction(mut self, direction: PeerMediaDirection) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        self.peer_direction = direction;
        Ok(self)
    }

    pub fn mute(mut self, direction: MuteDirection) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        self.mute = self.mute.mute(direction);
        Ok(self)
    }

    pub fn unmute(mut self, direction: MuteDirection) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        self.mute = self.mute.unmute(direction);
        Ok(self)
    }

    pub const fn media_connection(self) -> MediaConnection {
        self.connection
    }

    pub const fn connection_claim(self) -> ConnectionClaim {
        self.connection_claim
    }

    pub fn claim_connection(mut self) -> Result<Self, CoreError> {
        match self.connection_claim {
            ConnectionClaim::Available => {
                self.connection_claim = ConnectionClaim::Claimed;
                Ok(self)
            }
            ConnectionClaim::Claimed => Err(CoreError::ConnectionClaimed),
            ConnectionClaim::Released => Err(CoreError::Ended),
        }
    }

    pub fn connect_media(mut self) -> Decision {
        let transition = match self.connection {
            MediaConnection::Pending => {
                self.connection = MediaConnection::Connected;
                Transition::Notify
            }
            MediaConnection::Connected | MediaConnection::Released => Transition::Ignore,
        };
        Decision {
            call: self,
            transition,
        }
    }

    pub const fn bridge(self) -> Option<u64> {
        self.bridge
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub fn apply(mut self, event: CallEvent) -> Decision {
        let transition = match (self.state, event) {
            (
                CallState::Ended,
                CallEvent::Ring
                | CallEvent::Answer
                | CallEvent::Cancel
                | CallEvent::Hangup
                | CallEvent::Timeout,
            ) => Transition::Ignore,
            (
                CallState::Down | CallState::Ringing | CallState::Up,
                CallEvent::Cancel | CallEvent::Hangup | CallEvent::Timeout,
            ) => {
                self.state = CallState::Ended;
                self.media = MediaState::Released;
                self.connection = MediaConnection::Released;
                self.connection_claim = ConnectionClaim::Released;
                self.mute = AudioGate::Closed;
                self.bridge = None;
                self.stasis = StasisMembership::Released;
                Transition::Release
            }
            (CallState::Down, CallEvent::Ring) => {
                self.state = CallState::Ringing;
                Transition::Notify
            }
            (CallState::Down | CallState::Ringing, CallEvent::Answer) => {
                self.state = CallState::Up;
                self.media = MediaState::Active;
                Transition::Connect
            }
            (CallState::Ringing | CallState::Up, CallEvent::Ring)
            | (CallState::Up, CallEvent::Answer) => Transition::Ignore,
        };
        Decision {
            call: self,
            transition,
        }
    }

    pub fn hold(mut self) -> Result<Self, CoreError> {
        match self.state {
            CallState::Ended => Err(CoreError::Ended),
            CallState::Down | CallState::Ringing => Err(CoreError::NotAnswered),
            CallState::Up => {
                self.media = MediaState::Held;
                Ok(self)
            }
        }
    }

    pub fn resume(mut self) -> Result<Self, CoreError> {
        match self.media {
            MediaState::Released => Err(CoreError::Ended),
            MediaState::Waiting | MediaState::Active => Err(CoreError::NotHeld),
            MediaState::Held => {
                self.media = MediaState::Active;
                Ok(self)
            }
        }
    }

    pub fn join(mut self, bridge: u64) -> Result<Self, CoreError> {
        match self.state {
            CallState::Ended => return Err(CoreError::Ended),
            CallState::Down | CallState::Ringing => return Err(CoreError::NotAnswered),
            CallState::Up => {}
        }
        if self.bridge.is_some() {
            return Err(CoreError::AlreadyBridged);
        }
        self.bridge = Some(bridge);
        Ok(self)
    }

    pub fn leave(mut self, bridge: u64) -> Result<Self, CoreError> {
        if self.bridge != Some(bridge) {
            return Err(CoreError::WrongBridge);
        }
        self.bridge = None;
        Ok(self)
    }
}
