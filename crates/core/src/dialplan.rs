#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialplanInstruction {
    NoOp,
    Set,
    Answer,
    Dial,
    Stasis,
    Playback,
    Wait(u32),
    Goto,
    Hangup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialplanToken {
    generation: u64,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialplanError {
    Program,
    Clock,
    Capacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialplanContinuation {
    Advance,
    Jump(u16),
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialplanExecution {
    pub position: u16,
    pub token: DialplanToken,
    pub instruction: DialplanInstruction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialplanDispatch {
    Execute(DialplanExecution),
    Suspended,
    Yield,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialplanState {
    Ready { position: u16 },
    Awaiting { position: u16, token: DialplanToken },
    Waiting { position: u16, deadline: u64 },
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Awaiting {
        token: DialplanToken,
        next: Option<u16>,
    },
    Waiting {
        deadline: u64,
        next: Option<u16>,
    },
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialplanMachine {
    generation: u64,
    position: u16,
    length: u16,
    issued: u64,
    budget: u8,
    phase: Phase,
}

impl DialplanMachine {
    pub fn new(generation: u64, entry: u16, length: u16) -> Result<Self, DialplanError> {
        if length == 0 || length > 4096 || entry >= length {
            return Err(DialplanError::Program);
        }
        Ok(Self {
            generation,
            position: entry,
            length,
            issued: 0,
            budget: 32,
            phase: Phase::Ready,
        })
    }

    pub const fn state(&self) -> DialplanState {
        match self.phase {
            Phase::Ready => DialplanState::Ready {
                position: self.position,
            },
            Phase::Awaiting { token, next: _ } => DialplanState::Awaiting {
                position: self.position,
                token,
            },
            Phase::Waiting { deadline, next: _ } => DialplanState::Waiting {
                position: self.position,
                deadline,
            },
            Phase::Ended => DialplanState::Ended,
        }
    }

    pub fn begin_turn(&mut self) {
        match self.phase {
            Phase::Ready | Phase::Awaiting { .. } | Phase::Waiting { .. } => self.budget = 32,
            Phase::Ended => {}
        }
    }

    pub fn dispatch(
        &mut self,
        instruction: DialplanInstruction,
        next: Option<u16>,
        now: u64,
    ) -> Result<DialplanDispatch, DialplanError> {
        match self.phase {
            Phase::Ended => return Ok(DialplanDispatch::Ended),
            Phase::Awaiting { .. } | Phase::Waiting { .. } => {
                return Ok(DialplanDispatch::Suspended)
            }
            Phase::Ready => {}
        }
        if next.is_some_and(|next| next >= self.length)
            || instruction == DialplanInstruction::Goto && next.is_none()
        {
            return Err(DialplanError::Program);
        }
        if self.budget == 0 {
            return Ok(DialplanDispatch::Yield);
        }
        let issued = self.issued.checked_add(1).ok_or(DialplanError::Capacity)?;
        let token = DialplanToken {
            generation: self.generation,
            sequence: issued,
        };
        let phase = match instruction {
            DialplanInstruction::Wait(duration) => Phase::Waiting {
                deadline: now
                    .checked_add(u64::from(duration))
                    .ok_or(DialplanError::Clock)?,
                next,
            },
            DialplanInstruction::Hangup => Phase::Ended,
            DialplanInstruction::NoOp
            | DialplanInstruction::Set
            | DialplanInstruction::Answer
            | DialplanInstruction::Dial
            | DialplanInstruction::Stasis
            | DialplanInstruction::Playback
            | DialplanInstruction::Goto => Phase::Awaiting { token, next },
        };
        self.issued = issued;
        self.budget -= 1;
        self.phase = phase;
        Ok(DialplanDispatch::Execute(DialplanExecution {
            position: self.position,
            token,
            instruction,
        }))
    }

    pub fn complete(&mut self, token: DialplanToken, continuation: DialplanContinuation) -> bool {
        if matches!(continuation, DialplanContinuation::Jump(position) if position >= self.length) {
            return false;
        }
        match self.phase {
            Phase::Awaiting {
                token: expected,
                next,
            } if token == expected => {
                match continuation {
                    DialplanContinuation::Advance => self.advance(next),
                    DialplanContinuation::Jump(position) => self.advance(Some(position)),
                    DialplanContinuation::End => self.cancel(),
                }
                true
            }
            Phase::Ready | Phase::Awaiting { .. } | Phase::Waiting { .. } | Phase::Ended => false,
        }
    }

    pub fn tick(&mut self, now: u64) {
        match self.phase {
            Phase::Waiting { deadline, next } if now >= deadline => self.advance(next),
            Phase::Ready | Phase::Awaiting { .. } | Phase::Waiting { .. } | Phase::Ended => {}
        }
    }

    pub fn cancel(&mut self) {
        self.phase = Phase::Ended;
    }

    fn advance(&mut self, next: Option<u16>) {
        match next {
            Some(next) => {
                self.position = next;
                self.phase = Phase::Ready;
            }
            None => self.phase = Phase::Ended,
        }
    }
}
