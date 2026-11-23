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
        let elapsed = instant.saturating_duration_since(self.epoch);
        let whole = Self::millis(elapsed)?;
        if elapsed.subsec_nanos().is_multiple_of(1_000_000) {
            Ok(whole)
        } else {
            whole.checked_add(1).ok_or(ControlError::Capacity)
        }
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

#[cfg(test)]
mod tests {
    use super::Clock;
    use crate::ControlError;
    use rusterisk_core::Wake;
    use std::time::Duration;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn an_instant_inside_a_millisecond_reports_the_next_millisecond(
    ) -> Result<(), ControlError> {
        let clock = Clock::start();
        let epoch = Instant::now();
        assert_eq!(clock.at(epoch)?, 0);
        assert_eq!(clock.at(epoch + Duration::from_millis(2))?, 2);
        assert_eq!(clock.at(epoch + Duration::from_micros(1250))?, 2);
        assert_eq!(clock.at(epoch + Duration::from_nanos(1))?, 1);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_reported_instant_is_reached_no_earlier_than_its_source() -> Result<(), ControlError>
    {
        let clock = Clock::start();
        let deadline = Instant::now() + Duration::from_micros(7400);
        clock.reached(Wake::At(clock.at(deadline)?)).await;
        assert!(Instant::now() >= deadline);
        assert_eq!(clock.now_ms()?, 8);
        Ok(())
    }
}
