#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsGeneration {
    owner: u128,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsCandidate {
    owner: u128,
    sequence: u64,
}

impl SettingsGeneration {
    /// Compares the domain and sequence without establishing token liveness.
    pub const fn matches_candidate(self, candidate: SettingsCandidate) -> bool {
        self.owner == candidate.owner && self.sequence == candidate.sequence
    }
}

impl SettingsCandidate {
    /// Checks domain membership and sequence order without establishing token liveness.
    pub const fn follows(self, generation: SettingsGeneration) -> bool {
        self.owner == generation.owner && self.sequence > generation.sequence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsSlot {
    index: usize,
}

impl SettingsSlot {
    pub const fn index(self) -> usize {
        self.index
    }
}

#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub struct SettingsReference {
    generation: SettingsGeneration,
}

impl SettingsReference {
    pub const fn generation(&self) -> SettingsGeneration {
        self.generation
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SettingsError {
    Capacity,
    UnknownGeneration,
    UnknownCandidate,
    ReferenceCapacity,
    ForeignReference(SettingsReference),
    Unreferenced(SettingsReference),
}

impl SettingsError {
    pub fn into_reference(self) -> Option<SettingsReference> {
        match self {
            Self::ForeignReference(reference) | Self::Unreferenced(reference) => Some(reference),
            Self::Capacity
            | Self::UnknownGeneration
            | Self::UnknownCandidate
            | Self::ReferenceCapacity => None,
        }
    }
}

pub struct SettingsPublication {
    generation: SettingsGeneration,
    slot: SettingsSlot,
    retired: Option<SettingsSlot>,
}

impl SettingsPublication {
    pub const fn generation(&self) -> SettingsGeneration {
        self.generation
    }

    pub const fn slot(&self) -> SettingsSlot {
        self.slot
    }

    pub const fn retired_slot(&self) -> Option<SettingsSlot> {
        self.retired
    }
}

#[derive(Clone, Copy)]
struct Generation {
    key: SettingsGeneration,
    references: u64,
}

pub struct SettingsLifetimes {
    sequence: u64,
    current: SettingsGeneration,
    slots: [Option<Generation>; Self::CAPACITY],
    candidate: Option<SettingsCandidate>,
}

impl SettingsLifetimes {
    pub const CAPACITY: usize = 65;

    /// Callers must use a distinct owner for each settings lifetime domain.
    pub const fn new(owner: u128) -> Self {
        let current = SettingsGeneration { owner, sequence: 1 };
        let mut slots = [None; Self::CAPACITY];
        slots[0] = Some(Generation {
            key: current,
            references: 0,
        });
        Self {
            sequence: 1,
            current,
            slots,
            candidate: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn saturated_publication_state(
        owner: u128,
    ) -> (
        Self,
        [Option<SettingsReference>; Self::CAPACITY],
        SettingsCandidate,
    ) {
        let first = SettingsGeneration { owner, sequence: 1 };
        let mut slots = [Some(Generation {
            key: first,
            references: 1,
        }); Self::CAPACITY];
        let mut references = [const { None }; Self::CAPACITY];
        let mut index = 0_usize;
        let mut sequence = 1_u64;
        while index < Self::CAPACITY {
            let generation = SettingsGeneration { owner, sequence };
            slots[index] = Some(Generation {
                key: generation,
                references: 1,
            });
            references[index] = Some(SettingsReference { generation });
            index = index.saturating_add(1);
            sequence = sequence.saturating_add(1);
        }
        let current = SettingsGeneration {
            owner,
            sequence: sequence.saturating_sub(1),
        };
        let candidate = SettingsCandidate { owner, sequence };
        (
            Self {
                sequence: candidate.sequence,
                current,
                slots,
                candidate: Some(candidate),
            },
            references,
            candidate,
        )
    }

    #[cfg(kani)]
    pub(crate) fn reference_count_state(
        owner: u128,
        references: u64,
    ) -> (Self, SettingsGeneration) {
        let mut settings = Self::new(owner);
        let generation = settings.current;
        settings.slots[0] = Some(Generation {
            key: generation,
            references,
        });
        (settings, generation)
    }

    #[cfg(kani)]
    pub(crate) fn current_reference_count(&self) -> Option<u64> {
        self.slots[0].map(|retained| retained.references)
    }

    #[cfg(test)]
    pub(crate) fn matches_saturated_publication(
        &self,
        owner: u128,
        released_index: usize,
        publication: &SettingsPublication,
    ) -> bool {
        if released_index >= Self::CAPACITY - 1
            || self.current != publication.generation
            || publication.slot.index != released_index
            || publication.retired.is_some()
            || self.candidate.is_some()
        {
            return false;
        }
        let mut sequence = 0_u64;
        for (index, slot) in self.slots.iter().enumerate() {
            sequence = sequence.saturating_add(1);
            let Some(generation) = slot else {
                return false;
            };
            let (key, references) = if index == released_index {
                (publication.generation, 0)
            } else {
                (SettingsGeneration { owner, sequence }, 1)
            };
            if generation.key != key || generation.references != references {
                return false;
            }
        }
        self.sequence == publication.generation.sequence
            && publication.generation.owner == owner
            && publication.generation.sequence == sequence.saturating_add(1)
    }

    pub const fn current(&self) -> SettingsGeneration {
        self.current
    }

    pub fn slot(&self, generation: SettingsGeneration) -> Result<SettingsSlot, SettingsError> {
        self.slots
            .iter()
            .position(|slot| slot.is_some_and(|slot| slot.key == generation))
            .map(|index| SettingsSlot { index })
            .ok_or(SettingsError::UnknownGeneration)
    }

    fn available(&self) -> Result<SettingsSlot, SettingsError> {
        self.slots
            .iter()
            .position(|slot| {
                slot.is_none_or(|slot| slot.key == self.current && slot.references == 0)
            })
            .map(|index| SettingsSlot { index })
            .ok_or(SettingsError::Capacity)
    }

    pub fn prepare(&mut self) -> Result<SettingsCandidate, SettingsError> {
        if self.candidate.is_some() {
            return Err(SettingsError::Capacity);
        }
        self.available()?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(SettingsError::Capacity)?;
        let candidate = SettingsCandidate {
            owner: self.current.owner,
            sequence,
        };
        self.sequence = sequence;
        self.candidate = Some(candidate);
        Ok(candidate)
    }

    pub fn cancel(&mut self, candidate: SettingsCandidate) -> Result<(), SettingsError> {
        if self.candidate != Some(candidate) {
            return Err(SettingsError::UnknownCandidate);
        }
        self.candidate = None;
        Ok(())
    }

    pub fn commit(
        &mut self,
        candidate: SettingsCandidate,
    ) -> Result<SettingsPublication, SettingsError> {
        if self.candidate != Some(candidate) {
            return Err(SettingsError::UnknownCandidate);
        }
        let available = self.available()?;
        let current = self.slot(self.current)?;
        let previous = self.slots[current.index].ok_or(SettingsError::UnknownGeneration)?;
        let retired = if previous.references == 0 {
            self.slots[current.index] = None;
            Some(current)
        } else {
            None
        };
        let generation = SettingsGeneration {
            owner: candidate.owner,
            sequence: candidate.sequence,
        };
        self.slots[available.index] = Some(Generation {
            key: generation,
            references: 0,
        });
        self.current = generation;
        self.candidate = None;
        Ok(SettingsPublication {
            generation,
            slot: available,
            retired,
        })
    }

    pub fn retain(
        &mut self,
        generation: SettingsGeneration,
    ) -> Result<SettingsReference, SettingsError> {
        let slot = self.slot(generation)?;
        let retained = self.slots[slot.index]
            .as_mut()
            .ok_or(SettingsError::UnknownGeneration)?;
        retained.references = retained
            .references
            .checked_add(1)
            .ok_or(SettingsError::ReferenceCapacity)?;
        Ok(SettingsReference { generation })
    }

    pub fn release(
        &mut self,
        reference: SettingsReference,
    ) -> Result<Option<SettingsSlot>, SettingsError> {
        let Some((index, retained)) =
            self.slots.iter_mut().enumerate().find_map(|(index, slot)| {
                slot.as_mut()
                    .filter(|retained| retained.key == reference.generation)
                    .map(|retained| (index, retained))
            })
        else {
            return Err(SettingsError::ForeignReference(reference));
        };
        let Some(remaining) = retained.references.checked_sub(1) else {
            return Err(SettingsError::Unreferenced(reference));
        };
        retained.references = remaining;
        if retained.references == 0 && retained.key != self.current {
            self.slots[index] = None;
            Ok(Some(SettingsSlot { index }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(any(test, doctest))]
#[path = "settings_contracts.rs"]
mod contracts;

#[cfg(test)]
mod tests {
    use super::{SettingsCandidate, SettingsError, SettingsLifetimes, SettingsReference};

    fn reachable_saturated_publication(
        owner: u128,
    ) -> Result<
        (
            SettingsLifetimes,
            [Option<SettingsReference>; SettingsLifetimes::CAPACITY],
            SettingsCandidate,
        ),
        SettingsError,
    > {
        let mut settings = SettingsLifetimes::new(owner);
        let mut references: [Option<SettingsReference>; SettingsLifetimes::CAPACITY] =
            [const { None }; SettingsLifetimes::CAPACITY];
        for reference in references.iter_mut().take(SettingsLifetimes::CAPACITY - 1) {
            *reference = Some(settings.retain(settings.current())?);
            let candidate = settings.prepare()?;
            settings.commit(candidate)?;
        }
        let candidate = settings.prepare()?;
        references[SettingsLifetimes::CAPACITY - 1] = Some(settings.retain(settings.current())?);
        Ok((settings, references, candidate))
    }

    #[test]
    fn generation_exhaustion_refuses_preparation_without_changing_publication() {
        let mut settings = SettingsLifetimes::new(1);
        let current = settings.current();
        settings.sequence = u64::MAX;
        assert_eq!(settings.prepare(), Err(SettingsError::Capacity));
        assert_eq!(settings.current(), current);
        assert!(settings.candidate.is_none());
    }

    #[test]
    fn reference_exhaustion_keeps_the_published_generation() -> Result<(), SettingsError> {
        let mut settings = SettingsLifetimes::new(1);
        let current = settings.current();
        settings.slots[0]
            .as_mut()
            .ok_or(SettingsError::UnknownGeneration)?
            .references = u64::MAX;
        assert!(matches!(
            settings.retain(current),
            Err(SettingsError::ReferenceCapacity)
        ));
        assert_eq!(settings.current(), current);
        assert_eq!(
            settings.slots[0].map(|slot| slot.references),
            Some(u64::MAX)
        );
        Ok(())
    }

    #[test]
    fn saturated_assurance_state_matches_the_reachable_publication_state(
    ) -> Result<(), SettingsError> {
        let owner = 17;
        let (reachable, references, candidate) = reachable_saturated_publication(owner)?;
        let (modeled, modeled_references, modeled_candidate) =
            SettingsLifetimes::saturated_publication_state(owner);

        assert_eq!(candidate, modeled_candidate);
        assert_eq!(reachable.sequence, modeled.sequence);
        assert_eq!(reachable.current, modeled.current);
        assert_eq!(reachable.candidate, modeled.candidate);
        for (actual, expected) in reachable.slots.iter().zip(&modeled.slots) {
            assert_eq!(
                actual.map(|generation| (generation.key, generation.references)),
                expected.map(|generation| (generation.key, generation.references))
            );
        }
        for (actual, expected) in references.iter().zip(&modeled_references) {
            assert_eq!(
                actual.as_ref().map(SettingsReference::generation),
                expected.as_ref().map(SettingsReference::generation)
            );
        }
        Ok(())
    }

    #[test]
    fn every_retired_slot_is_reused_without_invalidating_other_references(
    ) -> Result<(), SettingsError> {
        let owner = 23;
        for released_index in 0..SettingsLifetimes::CAPACITY - 1 {
            let (mut settings, mut references, candidate) = reachable_saturated_publication(owner)?;
            let released = references[released_index]
                .take()
                .ok_or(SettingsError::Capacity)?;
            assert!(settings.release(released)?.is_some());
            let publication = settings.commit(candidate)?;
            assert!(settings.matches_saturated_publication(owner, released_index, &publication));
        }
        Ok(())
    }

    #[test]
    fn an_unreferenced_handle_survives_rejection_without_changing_the_slot(
    ) -> Result<(), SettingsError> {
        let mut settings = SettingsLifetimes::new(1);
        let generation = settings.current();
        let slot = settings.slot(generation)?;
        let reference = SettingsReference { generation };
        let Err(error) = settings.release(reference) else {
            return Err(SettingsError::UnknownGeneration);
        };
        assert!(matches!(&error, SettingsError::Unreferenced(_)));
        let recovered = error
            .into_reference()
            .ok_or(SettingsError::UnknownGeneration)?;
        assert_eq!(recovered.generation(), generation);
        assert_eq!(settings.slot(generation)?, slot);
        assert_eq!(
            settings.slots[slot.index].map(|retained| retained.references),
            Some(0)
        );
        let Err(repeated) = settings.release(recovered) else {
            return Err(SettingsError::UnknownGeneration);
        };
        assert!(matches!(&repeated, SettingsError::Unreferenced(_)));
        assert_eq!(
            repeated
                .into_reference()
                .map(|reference| reference.generation()),
            Some(generation)
        );
        assert_eq!(settings.current(), generation);
        Ok(())
    }
}
