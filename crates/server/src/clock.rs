use crate::ControlError;
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
        u64::try_from(self.epoch.elapsed().as_millis()).map_err(|_| ControlError::Capacity)
    }
}
