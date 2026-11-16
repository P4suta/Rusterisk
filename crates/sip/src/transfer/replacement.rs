use super::{ReferAuthority, ReferFailure, Replaces};
use crate::Engine;
#[cfg(test)]
#[path = "replacement_tests.rs"]
mod tests;

#[derive(Clone)]
pub struct ReplacementDialog {
    id: String,
    generation: u64,
    authority: ReferAuthority,
}

impl std::fmt::Debug for ReplacementDialog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplacementDialog")
            .finish_non_exhaustive()
    }
}

impl ReplacementDialog {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn authority(&self) -> &ReferAuthority {
        &self.authority
    }
}

pub(crate) enum Eligibility {
    Confirmed,
    Ended,
    Unsupported,
}

#[derive(Default)]
pub(crate) enum Selection {
    #[default]
    Missing,
    Found(Result<ReplacementDialog, ReferFailure>),
    Ambiguous,
}

impl Selection {
    pub fn include(
        &mut self,
        selector: &Replaces,
        id: &str,
        generation: u64,
        authority: &ReferAuthority,
        eligibility: Eligibility,
    ) {
        if !matches!(self, Self::Missing) {
            *self = Self::Ambiguous;
            return;
        }
        *self = Self::Found(match eligibility {
            Eligibility::Ended => Err(ReferFailure::Declined),
            Eligibility::Unsupported => Err(ReferFailure::Unsupported),
            Eligibility::Confirmed if selector.early_only() => Err(ReferFailure::Busy),
            Eligibility::Confirmed => Ok(ReplacementDialog {
                id: id.to_owned(),
                generation,
                authority: authority.clone(),
            }),
        });
    }

    fn finish(self) -> Result<Option<ReplacementDialog>, ReferFailure> {
        match self {
            Self::Missing => Ok(None),
            Self::Found(result) => result.map(Some),
            Self::Ambiguous => Err(ReferFailure::NotFound),
        }
    }
}

impl Engine {
    pub fn replacement_dialog(
        &self,
        selector: &Replaces,
    ) -> Result<Option<ReplacementDialog>, ReferFailure> {
        let mut selected = Selection::default();
        self.incoming_replacement(selector, &mut selected)
            .map_err(|_| ReferFailure::Internal)?;
        self.outgoing_replacement(selector, &mut selected)
            .map_err(|_| ReferFailure::Internal)?;
        selected.finish()
    }
}
