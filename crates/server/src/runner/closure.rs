use crate::ControlError;
use tokio::sync::mpsc;

pub(super) const CAPACITY: usize = 128;

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
