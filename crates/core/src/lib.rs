#![no_std]

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
    Ended,
    NotAnswered,
    NotHeld,
    AlreadyBridged,
    WrongBridge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    generation: u64,
    state: CallState,
    media: MediaState,
    bridge: Option<u64>,
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
            bridge: None,
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
                self.bridge = None;
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
