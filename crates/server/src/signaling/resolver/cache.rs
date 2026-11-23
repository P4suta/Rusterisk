use super::ResolvedTarget;
use crate::ControlError;
use rusterisk_sip::StaticTarget;
use tokio::time::Instant;

#[cfg(test)]
mod tests;

pub(crate) struct ResolutionCache {
    entries: [Option<ResolvedTarget>; 64],
    cursor: u8,
}

impl Default for ResolutionCache {
    fn default() -> Self {
        Self {
            entries: [const { None }; 64],
            cursor: 0,
        }
    }
}

impl ResolutionCache {
    fn expire(&mut self, now: Instant) {
        for entry in &mut self.entries {
            if entry
                .as_ref()
                .is_some_and(|result| !result.cache_valid(now))
            {
                *entry = None;
            }
        }
    }

    pub(crate) fn get(&mut self, target: &StaticTarget, now: Instant) -> Option<ResolvedTarget> {
        self.expire(now);
        self.entries
            .iter()
            .flatten()
            .find(|result| &result.target == target)
            .cloned()
    }

    pub(crate) fn insert(
        &mut self,
        result: ResolvedTarget,
        now: Instant,
    ) -> Result<(), ControlError> {
        self.expire(now);
        if !result.cache_valid(now) {
            return Ok(());
        }
        let index = self
            .entries
            .iter()
            .position(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|entry| entry.target == result.target)
            })
            .or_else(|| self.entries.iter().position(Option::is_none))
            .unwrap_or(usize::from(self.cursor));
        *self.entries.get_mut(index).ok_or(ControlError::Capacity)? = Some(result);
        self.cursor = u8::try_from((index + 1) % 64).map_err(|_| ControlError::Capacity)?;
        Ok(())
    }
}
