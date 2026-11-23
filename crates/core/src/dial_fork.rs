use crate::{DialplanToken, Wake};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialForkError {
    Capacity,
    Clock,
    Branch,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialForkState {
    Dialing { pending: u16 },
    Bridged { winner: u8 },
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialForkOutcome {
    Pending,
    Connect { winner: u8 },
    Continue,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialForkDecision {
    pub release: u16,
    pub outcome: DialForkOutcome,
}

impl DialForkDecision {
    const fn pending(release: u16) -> Self {
        Self {
            release,
            outcome: DialForkOutcome::Pending,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialFork {
    scope: DialplanToken,
    branches: u8,
    deadline: Option<u64>,
    state: DialForkState,
}

impl DialFork {
    pub fn new(
        scope: DialplanToken,
        branches: u8,
        now: u64,
        timeout: Option<u32>,
    ) -> Result<Self, DialForkError> {
        if branches == 0 || branches > 16 {
            return Err(DialForkError::Capacity);
        }
        let deadline = match timeout {
            Some(timeout) => Some(
                now.checked_add(u64::from(timeout))
                    .ok_or(DialForkError::Clock)?,
            ),
            None => None,
        };
        Ok(Self {
            scope,
            branches,
            deadline,
            state: DialForkState::Dialing {
                pending: u16::MAX >> (16 - branches),
            },
        })
    }

    pub const fn state(&self) -> DialForkState {
        self.state
    }

    pub const fn wake(&self) -> Wake {
        match (self.state, self.deadline) {
            (DialForkState::Dialing { .. }, Some(deadline)) => Wake::At(deadline),
            (DialForkState::Dialing { .. }, None)
            | (DialForkState::Bridged { .. } | DialForkState::Ended, _) => Wake::Idle,
        }
    }

    fn branch(&self, scope: DialplanToken, branch: u8) -> Result<u16, DialForkError> {
        if scope != self.scope {
            return Err(DialForkError::Stale);
        }
        if branch >= self.branches {
            return Err(DialForkError::Branch);
        }
        Ok(1 << branch)
    }

    pub fn answer(
        &mut self,
        scope: DialplanToken,
        branch: u8,
        now: u64,
    ) -> Result<DialForkDecision, DialForkError> {
        let bit = self.branch(scope, branch)?;
        let expired = self.tick(now);
        if expired.outcome == DialForkOutcome::Continue {
            return Ok(DialForkDecision {
                release: expired.release | bit,
                outcome: DialForkOutcome::Continue,
            });
        }
        Ok(match self.state {
            DialForkState::Dialing { pending } if pending & bit != 0 => {
                self.state = DialForkState::Bridged { winner: branch };
                DialForkDecision {
                    release: pending & !bit,
                    outcome: DialForkOutcome::Connect { winner: branch },
                }
            }
            DialForkState::Bridged { winner } if winner == branch => DialForkDecision::pending(0),
            DialForkState::Dialing { .. }
            | DialForkState::Bridged { .. }
            | DialForkState::Ended => DialForkDecision::pending(bit),
        })
    }

    pub fn failed(
        &mut self,
        scope: DialplanToken,
        branch: u8,
        now: u64,
    ) -> Result<DialForkDecision, DialForkError> {
        let bit = self.branch(scope, branch)?;
        let expired = self.tick(now);
        if expired.outcome == DialForkOutcome::Continue {
            return Ok(DialForkDecision {
                release: expired.release | bit,
                outcome: DialForkOutcome::Continue,
            });
        }
        Ok(match self.state {
            DialForkState::Dialing { pending } => {
                let pending = pending & !bit;
                self.state = if pending == 0 {
                    DialForkState::Ended
                } else {
                    DialForkState::Dialing { pending }
                };
                DialForkDecision {
                    release: bit,
                    outcome: if pending == 0 {
                        DialForkOutcome::Continue
                    } else {
                        DialForkOutcome::Pending
                    },
                }
            }
            DialForkState::Bridged { winner } if winner == branch => {
                self.state = DialForkState::Ended;
                DialForkDecision {
                    release: bit,
                    outcome: DialForkOutcome::End,
                }
            }
            DialForkState::Bridged { .. } | DialForkState::Ended => DialForkDecision::pending(bit),
        })
    }

    pub fn tick(&mut self, now: u64) -> DialForkDecision {
        match self.state {
            DialForkState::Dialing { pending }
                if self.deadline.is_some_and(|deadline| now >= deadline) =>
            {
                self.state = DialForkState::Ended;
                DialForkDecision {
                    release: pending,
                    outcome: DialForkOutcome::Continue,
                }
            }
            DialForkState::Dialing { .. }
            | DialForkState::Bridged { .. }
            | DialForkState::Ended => DialForkDecision::pending(0),
        }
    }

    pub fn cancel(&mut self) -> DialForkDecision {
        let release = match self.state {
            DialForkState::Dialing { pending } => pending,
            DialForkState::Bridged { winner } => 1 << winner,
            DialForkState::Ended => return DialForkDecision::pending(0),
        };
        self.state = DialForkState::Ended;
        DialForkDecision {
            release,
            outcome: DialForkOutcome::End,
        }
    }
}
