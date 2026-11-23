/// When a machine next needs its owner.
/// An instant counts milliseconds on the clock of the `now` values that the owner passes to the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The machine has work for the current turn.
    Now,
    /// The machine changes once the clock reaches this instant, so an earlier instant is already due.
    At(u64),
    /// Only an event method changes the machine, which may still be active.
    Idle,
}

impl Wake {
    /// The wake that comes first.
    #[must_use]
    pub const fn earliest(self, other: Self) -> Self {
        match (self, other) {
            (Self::Now, _) | (_, Self::Now) => Self::Now,
            (Self::At(first), Self::At(second)) => {
                if first <= second {
                    Self::At(first)
                } else {
                    Self::At(second)
                }
            }
            (Self::At(instant), Self::Idle) | (Self::Idle, Self::At(instant)) => Self::At(instant),
            (Self::Idle, Self::Idle) => Self::Idle,
        }
    }
}
