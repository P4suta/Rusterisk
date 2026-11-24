use tokio::sync::mpsc::{self, error::TrySendError};

#[derive(Clone)]
pub(crate) struct ClosureLatch {
    wake: mpsc::Sender<()>,
}

pub(super) fn latch() -> (ClosureLatch, mpsc::Receiver<()>) {
    let (wake, woken) = mpsc::channel(1);
    (ClosureLatch { wake }, woken)
}

impl ClosureLatch {
    fn set(&self) {
        match self.wake.try_send(()) {
            // A full latch holds a wake that the session reads after this closure.
            // A closed latch has no session to wake.
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Closed(())) => {}
        }
    }

    pub(super) fn notice(&self) -> ClosureNotice {
        ClosureNotice {
            latch: self.clone(),
        }
    }
}

// Drop this notice after the handles whose closure the session scans.
pub(super) struct ClosureNotice {
    latch: ClosureLatch,
}

impl ClosureNotice {
    pub(super) fn released(&self) {
        self.latch.set();
    }
}

impl Drop for ClosureNotice {
    fn drop(&mut self) {
        self.latch.set();
    }
}
