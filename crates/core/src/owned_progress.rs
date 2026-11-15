#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallMilestone {
    Accepted,
    Stasis,
    Answered,
    MediaConnected,
}

impl CallMilestone {
    const fn bit(self) -> u8 {
        match self {
            Self::Accepted => 1,
            Self::Stasis => 2,
            Self::Answered => 4,
            Self::MediaConnected => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallCompletion {
    Cancelled,
    Ended,
    StasisExited,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnedCallProgress {
    reached: u8,
    completion: Option<CallCompletion>,
}

impl OwnedCallProgress {
    pub const fn new() -> Self {
        Self {
            reached: 0,
            completion: None,
        }
    }
    pub const fn has_reached(self, milestone: CallMilestone) -> bool {
        self.reached & milestone.bit() != 0
    }
    pub const fn completion(self) -> Option<CallCompletion> {
        self.completion
    }
    pub const fn observe(self, milestone: CallMilestone) -> Self {
        match self.completion {
            None => Self {
                reached: self.reached | milestone.bit(),
                completion: None,
            },
            Some(
                CallCompletion::Cancelled
                | CallCompletion::Ended
                | CallCompletion::StasisExited
                | CallCompletion::Failed,
            ) => self,
        }
    }
    pub const fn finish(self, completion: CallCompletion) -> Self {
        match self.completion {
            None => Self {
                reached: self.reached,
                completion: Some(completion),
            },
            Some(
                CallCompletion::Cancelled
                | CallCompletion::Ended
                | CallCompletion::StasisExited
                | CallCompletion::Failed,
            ) => self,
        }
    }
}
