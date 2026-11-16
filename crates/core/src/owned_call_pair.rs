use crate::{Call, CallState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPairState {
    Linked,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPairError {
    Participants,
    NotAnswered,
    Bridge,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPairDecision {
    Ignore,
    ReleaseBridge { release: u64 },
    EndPeer { peer: u64, release: u64 },
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedCallPair {
    bridge: u64,
    generations: [u64; 2],
    state: CallPairState,
}

impl OwnedCallPair {
    pub fn new(left: Call, right: Call) -> Result<Self, CallPairError> {
        if left.generation() == right.generation() {
            return Err(CallPairError::Participants);
        }
        if left.state() != CallState::Up || right.state() != CallState::Up {
            return Err(CallPairError::NotAnswered);
        }
        let bridge = left.bridge().ok_or(CallPairError::Bridge)?;
        if right.bridge() != Some(bridge) {
            return Err(CallPairError::Bridge);
        }
        Ok(Self {
            bridge,
            generations: [left.generation(), right.generation()],
            state: CallPairState::Linked,
        })
    }

    pub const fn bridge(self) -> u64 {
        self.bridge
    }

    pub const fn generations(self) -> [u64; 2] {
        self.generations
    }

    pub const fn state(self) -> CallPairState {
        self.state
    }

    pub fn participant_ended(
        &mut self,
        bridge: u64,
        generation: u64,
        current_peer: Option<Call>,
    ) -> CallPairDecision {
        if self.state != CallPairState::Linked || bridge != self.bridge {
            return CallPairDecision::Ignore;
        }
        let peer = if generation == self.generations[0] {
            self.generations[1]
        } else if generation == self.generations[1] {
            self.generations[0]
        } else {
            return CallPairDecision::Ignore;
        };
        self.state = CallPairState::Released;
        match current_peer {
            Some(current)
                if current.generation() == peer
                    && current.bridge() == Some(self.bridge)
                    && current.state() == CallState::Up =>
            {
                CallPairDecision::EndPeer {
                    peer,
                    release: self.bridge,
                }
            }
            Some(_) | None => CallPairDecision::ReleaseBridge {
                release: self.bridge,
            },
        }
    }

    pub fn retire(&mut self, bridge: u64) -> CallPairDecision {
        if self.state != CallPairState::Linked || bridge != self.bridge {
            return CallPairDecision::Ignore;
        }
        self.state = CallPairState::Released;
        CallPairDecision::ReleaseBridge {
            release: self.bridge,
        }
    }
}
