use crate::ControlError;
use tokio::sync::mpsc;

const LIVE_SESSIONS: usize = 64;
const LIVE_LEASES: usize = 64;
pub(super) const CAPACITY: usize = 2 * (LIVE_SESSIONS + LIVE_LEASES);

pub(crate) struct ClosureNotice {
    permit: Option<mpsc::OwnedPermit<()>>,
}

impl ClosureNotice {
    pub(super) fn reserve(closures: &mpsc::Sender<()>) -> Result<Self, ControlError> {
        closures
            .clone()
            .try_reserve_owned()
            .map(|permit| Self {
                permit: Some(permit),
            })
            .map_err(|_| ControlError::Capacity)
    }
}

impl Drop for ClosureNotice {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            drop(permit.send(()));
        }
    }
}

pub(super) struct Abandonment<'a> {
    closures: &'a mpsc::Sender<()>,
    armed: bool,
}

impl<'a> Abandonment<'a> {
    pub(super) fn armed(closures: &'a mpsc::Sender<()>) -> Self {
        Self {
            closures,
            armed: true,
        }
    }

    pub(super) fn settle(&mut self, abandoned: bool) {
        self.armed = abandoned;
    }
}

impl Drop for Abandonment<'_> {
    fn drop(&mut self) {
        if self.armed && self.closures.try_send(()).is_err() {
            tracing::debug!("abandoned request cleanup waits for its deadline");
        }
    }
}
