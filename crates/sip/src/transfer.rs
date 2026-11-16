use rsipstack::sip::Request;
mod request;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferError {
    Invalid,
    Unsupported,
    Capacity,
}

impl std::fmt::Display for ReferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Invalid transfer request",
            Self::Unsupported => "Unsupported transfer request",
            Self::Capacity => "Transfer request capacity exceeded",
        })
    }
}

impl std::error::Error for ReferError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferSubscription {
    Notify,
    Suppressed,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BlindRefer {
    extension: String,
    subscription: ReferSubscription,
}

impl std::fmt::Debug for BlindRefer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlindRefer")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl BlindRefer {
    pub fn parse(request: &Request) -> Result<Self, ReferError> {
        request::parse(request)
    }

    pub fn extension(&self) -> &str {
        &self.extension
    }

    pub const fn subscription(&self) -> ReferSubscription {
        self.subscription
    }
}
