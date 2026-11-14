#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialFailure {
    Unavailable,
    Congestion,
    Busy,
    NoAnswer,
}

impl DialFailure {
    pub const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (
                Self::NoAnswer,
                Self::Unavailable | Self::Congestion | Self::Busy | Self::NoAnswer,
            )
            | (Self::Unavailable | Self::Congestion | Self::Busy, Self::NoAnswer) => Self::NoAnswer,
            (Self::Busy, Self::Unavailable | Self::Congestion | Self::Busy)
            | (Self::Unavailable | Self::Congestion, Self::Busy) => Self::Busy,
            (Self::Congestion, Self::Unavailable | Self::Congestion)
            | (Self::Unavailable, Self::Congestion) => Self::Congestion,
            (Self::Unavailable, Self::Unavailable) => Self::Unavailable,
        }
    }
}
