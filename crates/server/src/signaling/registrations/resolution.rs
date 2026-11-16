use super::{failure, ConfigurationKey};
use crate::{
    signaling::{
        resolver::{DestinationResolver, ResolvedTarget},
        transport::SetupKey,
    },
    ControlError,
};
use rusterisk_sip::StaticTarget;
use std::{num::NonZeroU64, time::Duration};
use tokio::sync::mpsc;

#[derive(Clone)]
pub(in crate::signaling) struct ConfigurationLease {
    generation: NonZeroU64,
    lifetime: mpsc::Sender<()>,
}

impl ConfigurationLease {
    pub(in crate::signaling) fn is_released(&self) -> bool {
        self.lifetime.is_closed()
    }

    pub(in crate::signaling) async fn released(&self) {
        self.lifetime.closed().await;
    }
}

pub(super) struct ConfigurationLifetime {
    generation: NonZeroU64,
    owner: mpsc::Receiver<()>,
    lease: ConfigurationLease,
}

impl ConfigurationLifetime {
    pub(super) fn new(generation: NonZeroU64) -> Self {
        let (lifetime, owner) = mpsc::channel(1);
        Self {
            generation,
            owner,
            lease: ConfigurationLease {
                generation,
                lifetime,
            },
        }
    }

    pub(super) fn lease(&self) -> ConfigurationLease {
        self.lease.clone()
    }

    pub(super) fn is_current(&self, key: ConfigurationKey, lease: &ConfigurationLease) -> bool {
        key.generation == self.generation
            && lease.generation == self.generation
            && !lease.is_released()
    }

    pub(super) fn cancel(&mut self) {
        self.owner.close();
    }

    pub(super) const fn generation(&self) -> NonZeroU64 {
        self.generation
    }
}

pub(in crate::signaling) struct Completion {
    pub(super) key: ConfigurationKey,
    pub(super) setup: SetupKey,
    pub(super) lease: ConfigurationLease,
    pub(super) result: Result<ResolvedTarget, ControlError>,
}

pub(super) async fn resolve(
    key: ConfigurationKey,
    setup: SetupKey,
    lease: ConfigurationLease,
    target: StaticTarget,
    cached: Option<ResolvedTarget>,
    resolver: Result<DestinationResolver, ControlError>,
) -> Completion {
    let lookup = async {
        match cached.or(ResolvedTarget::literal(&target)?) {
            Some(resolved) => Ok(resolved),
            None => resolver?.resolve(&target).await,
        }
    };
    let result = tokio::select! {
        _released = lease.released() => Err(ControlError::Conflict("Registration configuration ended during resolution")),
        resolved = tokio::time::timeout(Duration::from_secs(3), lookup) => resolved.map_err(failure).and_then(|result| result),
    };
    Completion {
        key,
        setup,
        lease,
        result,
    }
}
