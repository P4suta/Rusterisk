use super::ControlError;
use rusterisk_core::{
    SettingsCandidate, SettingsError, SettingsGeneration, SettingsLifetimes, SettingsReference,
};

pub(super) struct SettingsGenerations<T> {
    lifetimes: SettingsLifetimes,
    values: Vec<Option<T>>,
    candidate: Option<(SettingsCandidate, T)>,
}

fn failed(error: SettingsError) -> ControlError {
    match error {
        SettingsError::Capacity | SettingsError::ReferenceCapacity => ControlError::Capacity,
        SettingsError::UnknownCandidate => ControlError::Conflict("Unknown settings candidate"),
        SettingsError::UnknownGeneration
        | SettingsError::ForeignReference(_)
        | SettingsError::Unreferenced(_) => ControlError::Conflict("Unknown settings reference"),
    }
}

impl<T> SettingsGenerations<T> {
    pub(super) fn new(initial: T, owner: u128) -> Self {
        let mut values: Vec<_> = (0..SettingsLifetimes::CAPACITY).map(|_| None).collect();
        values[0] = Some(initial);
        Self {
            lifetimes: SettingsLifetimes::new(owner),
            values,
            candidate: None,
        }
    }

    pub(super) fn current(&self) -> SettingsGeneration {
        self.lifetimes.current()
    }

    pub(super) fn get(&self, generation: SettingsGeneration) -> Result<&T, ControlError> {
        let slot = self.lifetimes.slot(generation).map_err(failed)?;
        self.values[slot.index()]
            .as_ref()
            .ok_or(ControlError::Conflict("Settings payload is missing"))
    }

    pub(super) fn retain(
        &mut self,
        generation: SettingsGeneration,
    ) -> Result<SettingsReference, ControlError> {
        self.lifetimes.retain(generation).map_err(failed)
    }

    pub(super) fn release(&mut self, reference: SettingsReference) -> Result<(), ControlError> {
        if let Some(slot) = self.lifetimes.release(reference).map_err(failed)? {
            self.values[slot.index()] = None;
        }
        Ok(())
    }

    pub(super) fn prepare(&mut self, value: T) -> Result<SettingsCandidate, ControlError> {
        let key = self.lifetimes.prepare().map_err(failed)?;
        self.candidate = Some((key, value));
        Ok(key)
    }

    pub(super) fn cancel(&mut self, key: SettingsCandidate) -> Result<(), ControlError> {
        self.lifetimes.cancel(key).map_err(failed)?;
        self.candidate = None;
        Ok(())
    }

    pub(super) fn commit(
        &mut self,
        key: SettingsCandidate,
    ) -> Result<SettingsGeneration, ControlError> {
        if self.candidate.as_ref().map(|(candidate, _)| *candidate) != Some(key) {
            return Err(ControlError::Conflict("Unknown settings candidate"));
        }
        let pending = self
            .candidate
            .take()
            .ok_or(ControlError::Conflict("Settings candidate is missing"))?;
        match self.lifetimes.commit(key) {
            Ok(publication) => {
                if let Some(slot) = publication.retired_slot() {
                    self.values[slot.index()] = None;
                }
                self.values[publication.slot().index()] = Some(pending.1);
                Ok(publication.generation())
            }
            Err(error) => {
                self.candidate = Some(pending);
                Err(failed(error))
            }
        }
    }
}
