use crate::{Call, CallState, CoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StasisApplication(u64);

impl StasisApplication {
    pub const fn new(generation: u64) -> Self {
        Self(generation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StasisMembership {
    Outside,
    Pending(StasisApplication),
    Entered(StasisApplication),
    Released,
}

impl Call {
    pub const fn stasis(self) -> StasisMembership {
        self.stasis
    }

    pub fn reserve_stasis(mut self, application: StasisApplication) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        match self.stasis {
            StasisMembership::Outside => {
                self.stasis = StasisMembership::Pending(application);
                Ok(self)
            }
            StasisMembership::Pending(current) | StasisMembership::Entered(current)
                if current == application =>
            {
                Ok(self)
            }
            StasisMembership::Pending(_) | StasisMembership::Entered(_) => {
                Err(CoreError::StasisConflict)
            }
            StasisMembership::Released => Err(CoreError::Ended),
        }
    }

    pub fn enter_stasis(mut self, application: StasisApplication) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        match self.stasis {
            StasisMembership::Outside => {
                self.stasis = StasisMembership::Entered(application);
                Ok(self)
            }
            StasisMembership::Pending(current) if current == application => {
                if self.state != CallState::Up {
                    return Err(CoreError::NotAnswered);
                }
                self.stasis = StasisMembership::Entered(application);
                Ok(self)
            }
            StasisMembership::Entered(current) if current == application => Ok(self),
            StasisMembership::Pending(_) | StasisMembership::Entered(_) => {
                Err(CoreError::StasisConflict)
            }
            StasisMembership::Released => Err(CoreError::Ended),
        }
    }

    pub fn leave_stasis(mut self, application: StasisApplication) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        match self.stasis {
            StasisMembership::Entered(current) if current == application => {
                self.stasis = StasisMembership::Outside;
                Ok(self)
            }
            StasisMembership::Entered(_) => Err(CoreError::WrongStasisApplication),
            StasisMembership::Outside | StasisMembership::Pending(_) => Err(CoreError::NotInStasis),
            StasisMembership::Released => Err(CoreError::Ended),
        }
    }

    pub fn move_stasis(
        mut self,
        from: StasisApplication,
        to: StasisApplication,
    ) -> Result<Self, CoreError> {
        if self.state == CallState::Ended {
            return Err(CoreError::Ended);
        }
        match self.stasis {
            StasisMembership::Entered(current) if current == from => {
                self.stasis = StasisMembership::Entered(to);
                Ok(self)
            }
            StasisMembership::Entered(_) => Err(CoreError::WrongStasisApplication),
            StasisMembership::Outside | StasisMembership::Pending(_) => Err(CoreError::NotInStasis),
            StasisMembership::Released => Err(CoreError::Ended),
        }
    }
}
