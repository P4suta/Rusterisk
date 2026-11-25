use std::{future::IntoFuture, time::Duration};
use tokio::time::Timeout;

#[derive(Clone, Copy)]
pub enum Wait {
    Ready,
    Io,
    Scenario,
    Quiet,
    Observation,
    Responsive,
    Shutdown,
    Browser,
}

impl Wait {
    pub const fn duration(self) -> Duration {
        match self {
            Self::Ready => Duration::from_secs(5),
            Self::Io => Duration::from_secs(2),
            Self::Scenario => Duration::from_secs(10),
            Self::Quiet => Duration::from_millis(150),
            Self::Observation => Duration::from_millis(500),
            Self::Responsive => Duration::from_secs(1),
            Self::Shutdown => Duration::from_secs(35),
            Self::Browser => Duration::from_secs(150),
        }
    }

    pub fn bounded<F: IntoFuture>(self, future: F) -> Timeout<F::IntoFuture> {
        tokio::time::timeout(self.duration(), future)
    }
}
