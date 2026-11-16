use rsipstack::sip::Request;
mod authority;
mod delivery;
pub(crate) mod dialog;
pub(crate) mod notify;
pub(crate) mod owners;
mod progress;
mod request;
mod subscribe;
pub(crate) mod wire;
pub use authority::ReferAuthority;
pub use delivery::ReferIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReferKey(u64);

#[derive(Clone, Debug)]
pub struct DialogRefer {
    key: ReferKey,
    target: BlindRefer,
    authority: ReferAuthority,
}

impl DialogRefer {
    pub const fn key(&self) -> ReferKey {
        self.key
    }
    pub fn target(&self) -> &BlindRefer {
        &self.target
    }
    pub fn authority(&self) -> &ReferAuthority {
        &self.authority
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferFailure {
    NotFound,
    Forbidden,
    Unsupported,
    Unavailable,
    Busy,
    Timeout,
    Cancelled,
    Declined,
    Internal,
}

impl ReferFailure {
    pub const fn code(self) -> u16 {
        self.response().0
    }
    pub(crate) const fn response(self) -> (u16, &'static str) {
        match self {
            Self::NotFound => (404, "Not Found"),
            Self::Forbidden => (403, "Forbidden"),
            Self::Unsupported => (501, "Not Implemented"),
            Self::Unavailable => (503, "Service Unavailable"),
            Self::Busy => (486, "Busy Here"),
            Self::Timeout => (408, "Request Timeout"),
            Self::Cancelled => (487, "Request Terminated"),
            Self::Declined => (603, "Decline"),
            Self::Internal => (500, "Server Internal Error"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferProgress {
    Rejected(ReferFailure),
    Trying,
    HandedOff,
    Ringing,
    Answered,
    Failed(ReferFailure),
}

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
