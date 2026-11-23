#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Now,
    At(u64),
    Idle,
}

impl Wake {
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
