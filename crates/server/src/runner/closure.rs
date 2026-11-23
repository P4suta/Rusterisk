use std::time::Duration;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::oneshot;
use tokio::time::error::Elapsed;

#[cfg(test)]
#[path = "closure_tests.rs"]
mod tests;

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
            // A full latch holds a wake that the runner reads after this closure.
            // A closed latch has no runner to wake.
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Closed(())) => {}
        }
    }

    pub(super) fn notice(&self) -> ClosureNotice {
        ClosureNotice {
            latch: self.clone(),
        }
    }

    pub(super) async fn answered<T>(
        &self,
        budget: Duration,
        reply: oneshot::Receiver<T>,
    ) -> Result<Result<T, oneshot::error::RecvError>, Elapsed> {
        let mut wait = ReplyWait {
            reply,
            latch: self,
            ended: false,
        };
        let outcome = tokio::time::timeout(budget, &mut wait.reply).await;
        wait.ended = outcome.is_ok();
        outcome
    }
}

// Drop this notice after the state whose closure the runner scans.
pub(crate) struct ClosureNotice {
    latch: ClosureLatch,
}

impl Drop for ClosureNotice {
    fn drop(&mut self) {
        self.latch.set();
    }
}

struct ReplyWait<'a, T> {
    reply: oneshot::Receiver<T>,
    latch: &'a ClosureLatch,
    ended: bool,
}

impl<T> Drop for ReplyWait<'_, T> {
    fn drop(&mut self) {
        if !self.ended {
            self.reply.close();
            self.latch.set();
        }
    }
}
