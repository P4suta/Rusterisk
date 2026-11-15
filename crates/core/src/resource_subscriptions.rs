#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    Channel,
    Bridge,
    Endpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceSource {
    Specific { kind: ResourceKind, generation: u64 },
    All(ResourceKind),
}

impl ResourceSource {
    pub const fn kind(self) -> ResourceKind {
        match self {
            Self::Specific { kind, .. } | Self::All(kind) => kind,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionError {
    Capacity,
    Missing,
}

impl core::fmt::Display for SubscriptionError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::Capacity => "Resource subscription capacity exceeded",
            Self::Missing => "Resource source isn't subscribed",
        })
    }
}
impl core::error::Error for SubscriptionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interest {
    source: ResourceSource,
    count: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceSubscriptions<const N: usize> {
    interests: [Option<Interest>; N],
}

impl<const N: usize> Default for ResourceSubscriptions<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ResourceSubscriptions<N> {
    pub const fn new() -> Self {
        Self {
            interests: [None; N],
        }
    }

    pub fn sources(&self) -> impl Iterator<Item = ResourceSource> + '_ {
        self.interests
            .iter()
            .flatten()
            .map(|interest| interest.source)
    }

    pub fn interested_in(&self, source: ResourceSource) -> bool {
        self.sources()
            .any(|current| current == source || current == ResourceSource::All(source.kind()))
    }

    pub fn subscribe(&mut self, source: ResourceSource) -> Result<(), SubscriptionError> {
        if self
            .sources()
            .any(|current| current == ResourceSource::All(source.kind()))
        {
            return Ok(());
        }
        if let Some(interest) = self
            .interests
            .iter_mut()
            .flatten()
            .find(|interest| interest.source == source)
        {
            interest.count = interest
                .count
                .checked_add(1)
                .ok_or(SubscriptionError::Capacity)?;
            return Ok(());
        }
        let slot = self
            .interests
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(SubscriptionError::Capacity)?;
        *slot = Some(Interest { source, count: 1 });
        Ok(())
    }

    pub fn unsubscribe(&mut self, source: ResourceSource) -> Result<(), SubscriptionError> {
        let slot = self
            .interests
            .iter_mut()
            .find(|slot| slot.is_some_and(|interest| interest.source == source))
            .ok_or(SubscriptionError::Missing)?;
        if let Some(interest) = slot {
            if interest.count == 1 {
                *slot = None;
            } else {
                interest.count -= 1;
            }
        }
        Ok(())
    }

    pub fn expire(&mut self, source: ResourceSource) {
        match source {
            ResourceSource::Specific { .. } => {
                for slot in &mut self.interests {
                    if slot.is_some_and(|interest| interest.source == source) {
                        *slot = None;
                    }
                }
            }
            ResourceSource::All(_) => {}
        }
    }
}
