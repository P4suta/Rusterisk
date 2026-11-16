use super::{request, ReferError, ReferSubscription, Replaces};
use rsipstack::sip::{Request, Uri};

#[derive(Clone, PartialEq, Eq)]
pub struct AttendedRefer {
    pub(super) uri: Uri,
    pub(super) replaces: Replaces,
    pub(super) subscription: ReferSubscription,
}

impl std::fmt::Debug for AttendedRefer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttendedRefer")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl AttendedRefer {
    pub fn parse(request: &Request) -> Result<Self, ReferError> {
        request::parse_attended(request)
    }

    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    pub const fn replaces(&self) -> &Replaces {
        &self.replaces
    }

    pub const fn subscription(&self) -> ReferSubscription {
        self.subscription
    }
}
