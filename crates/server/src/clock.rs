use crate::ControlError;
use rusterisk_core::Wake;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    epoch: Instant,
}

impl Clock {
    pub(crate) fn start() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    pub(crate) fn now_ms(self) -> Result<u64, ControlError> {
        Self::millis(self.epoch.elapsed())
    }

    pub(crate) fn at(self, instant: Instant) -> Result<u64, ControlError> {
        Self::millis(instant.saturating_duration_since(self.epoch))
    }

    pub(crate) async fn reached(self, wake: Wake) {
        let instant = match wake {
            Wake::Now => return,
            Wake::At(at) => self.epoch.checked_add(Duration::from_millis(at)),
            Wake::Idle => None,
        };
        match instant {
            Some(instant) => tokio::time::sleep_until(instant).await,
            None => std::future::pending().await,
        }
    }

    fn millis(elapsed: Duration) -> Result<u64, ControlError> {
        u64::try_from(elapsed.as_millis()).map_err(|_| ControlError::Capacity)
    }
}
