use crate::{
    release_check::{
        CapturedReleaseSource, InputError, InputKey, LinuxOciTarget, RequiredGate, SourceInputs,
    },
    server_distribution::ServerTarget,
};
use std::{fmt, path::Path};

mod identities;
mod outcome;
mod profile;
mod projections;
use identities::{
    require_same_sdk, require_same_server, SdkArchiveIdentity, ServerArchiveIdentity,
};
pub use outcome::RunnerOutcome;
pub use profile::{CandidateImages, CandidateProfile};
use projections::ProjectionSet;

#[derive(Debug)]
pub enum CandidateCheckError {
    Missing(RequiredGate),
    Duplicate(RequiredGate),
    Inputs {
        gate: RequiredGate,
        error: InputError,
    },
    Projection(RequiredGate),
    Profile(RequiredGate),
    Identity(RequiredGate, &'static str),
    Source {
        stage: &'static str,
        cause: String,
    },
    Inventory,
}

impl fmt::Display for CandidateCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(gate) => write!(formatter, "required candidate check is missing: {gate}"),
            Self::Duplicate(gate) => {
                write!(formatter, "candidate check was already included: {gate}")
            }
            Self::Inputs { gate, error } => write!(formatter, "{gate} input binding: {error}"),
            Self::Projection(gate) => write!(
                formatter,
                "{gate} inputs differ from the complete captured projection"
            ),
            Self::Profile(gate) => write!(
                formatter,
                "{gate} immutable image or architecture differs from the selected profile"
            ),
            Self::Identity(gate, reason) => {
                write!(formatter, "{gate} candidate identity: {reason}")
            }
            Self::Source { stage, cause } => write!(formatter, "{stage}: {cause}"),
            Self::Inventory => formatter.write_str("candidate check inventory is inconsistent"),
        }
    }
}
impl std::error::Error for CandidateCheckError {}

fn index(gate: RequiredGate) -> Result<usize, CandidateCheckError> {
    RequiredGate::ALL
        .iter()
        .position(|required| *required == gate)
        .ok_or(CandidateCheckError::Inventory)
}

pub struct CandidateChecks<'run, 'source: 'run> {
    captured: &'source CapturedReleaseSource,
    inputs: SourceInputs<'source>,
    profile: CandidateProfile,
    projections: ProjectionSet,
    slots: [Option<RunnerOutcome<'run, 'source>>; RequiredGate::ALL.len()],
}

pub struct CompletedCandidateChecks<'run, 'source: 'run> {
    captured: &'source CapturedReleaseSource,
    profile: CandidateProfile,
    outcomes: [RunnerOutcome<'run, 'source>; RequiredGate::ALL.len()],
}

impl<'run, 'source: 'run> CandidateChecks<'run, 'source> {
    pub fn new(
        captured: &'source CapturedReleaseSource,
        profile: CandidateProfile,
    ) -> Result<Self, CandidateCheckError> {
        let inputs = captured
            .inputs()
            .map_err(|error| CandidateCheckError::Inputs {
                gate: RequiredGate::Fast,
                error,
            })?;
        Ok(Self {
            captured,
            inputs,
            profile,
            projections: ProjectionSet::new(),
            slots: [None; RequiredGate::ALL.len()],
        })
    }

    pub fn include(
        &mut self,
        outcome: RunnerOutcome<'run, 'source>,
    ) -> Result<(), CandidateCheckError> {
        let gate = outcome.gate();
        let position = index(gate)?;
        if self
            .slots
            .get(position)
            .ok_or(CandidateCheckError::Inventory)?
            .is_some()
        {
            return Err(CandidateCheckError::Duplicate(gate));
        }
        self.check_profile(outcome)?;
        let projection = outcome
            .inputs()
            .map_err(|error| CandidateCheckError::Inputs { gate, error })?;
        self.inputs
            .bind(&projection)
            .map_err(|error| CandidateCheckError::Inputs { gate, error })?;
        let expected = if outcome.inputs_are_release_source() {
            self.inputs.key()
        } else {
            self.projections.expected(self.captured, gate)?
        };
        if projection.key() != expected {
            return Err(CandidateCheckError::Projection(gate));
        }
        self.check_candidate_projections(outcome)?;
        *self
            .slots
            .get_mut(position)
            .ok_or(CandidateCheckError::Inventory)? = Some(outcome);
        Ok(())
    }

    fn check_profile(
        &self,
        outcome: RunnerOutcome<'run, 'source>,
    ) -> Result<(), CandidateCheckError> {
        let gate = outcome.gate();
        if outcome.image() != self.profile.image(gate) {
            return Err(CandidateCheckError::Profile(gate));
        }
        if let RunnerOutcome::InputFuzz(outcome) = outcome {
            if outcome.architecture() != self.profile.fuzz_architecture() {
                return Err(CandidateCheckError::Profile(gate));
            }
        }
        Ok(())
    }

    fn require_projection(
        &mut self,
        gate: RequiredGate,
        found: InputKey,
    ) -> Result<(), CandidateCheckError> {
        if self.projections.expected(self.captured, gate)? != found {
            return Err(CandidateCheckError::Projection(gate));
        }
        Ok(())
    }

    fn check_candidate_projections(
        &mut self,
        outcome: RunnerOutcome<'run, 'source>,
    ) -> Result<(), CandidateCheckError> {
        match outcome {
            RunnerOutcome::Sdk(outcome) => {
                self.require_projection(RequiredGate::Sdk, outcome.projection())
            }
            RunnerOutcome::Server(outcome) => {
                self.require_projection(
                    RequiredGate::Server(outcome.target()),
                    outcome.projection(),
                )?;
                self.require_projection(RequiredGate::Sdk, outcome.sdk_projection())
            }
            RunnerOutcome::Oci(outcome) => {
                self.require_projection(
                    RequiredGate::Server(outcome.target().server_target()),
                    outcome.projection(),
                )?;
                self.require_projection(RequiredGate::Sdk, outcome.sdk_projection())
            }
            RunnerOutcome::Fast(_)
            | RunnerOutcome::Security(_)
            | RunnerOutcome::Reference(_)
            | RunnerOutcome::Browser(_)
            | RunnerOutcome::CoreProofs(_)
            | RunnerOutcome::CoreMiri(_)
            | RunnerOutcome::CoreMutation(_)
            | RunnerOutcome::InputFuzz(_)
            | RunnerOutcome::NativeSanitizers(_) => Ok(()),
        }
    }

    fn outcome(
        &self,
        gate: RequiredGate,
    ) -> Result<RunnerOutcome<'run, 'source>, CandidateCheckError> {
        self.slots
            .get(index(gate)?)
            .copied()
            .flatten()
            .ok_or(CandidateCheckError::Missing(gate))
    }

    fn candidate_identities(&self) -> Result<(), CandidateCheckError> {
        let RunnerOutcome::Sdk(sdk) = self.outcome(RequiredGate::Sdk)? else {
            return Err(CandidateCheckError::Inventory);
        };
        let sdk = SdkArchiveIdentity::from_sdk(sdk)?;
        for target in ServerTarget::ALL {
            let gate = RequiredGate::Server(target);
            let RunnerOutcome::Server(server) = self.outcome(gate)? else {
                return Err(CandidateCheckError::Inventory);
            };
            require_same_sdk(gate, &sdk, &SdkArchiveIdentity::from_server(server)?)?;
        }
        for target in [LinuxOciTarget::X86_64, LinuxOciTarget::Arm64] {
            let gate = RequiredGate::Oci(target);
            let RunnerOutcome::Oci(oci) = self.outcome(gate)? else {
                return Err(CandidateCheckError::Inventory);
            };
            require_same_sdk(gate, &sdk, &SdkArchiveIdentity::from_oci(oci)?)?;
            let RunnerOutcome::Server(server) =
                self.outcome(RequiredGate::Server(target.server_target()))?
            else {
                return Err(CandidateCheckError::Inventory);
            };
            require_same_server(
                target,
                &ServerArchiveIdentity::from_server(server)?,
                &ServerArchiveIdentity::from_oci(oci)?,
            )?;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<CompletedCandidateChecks<'run, 'source>, CandidateCheckError> {
        for gate in RequiredGate::ALL {
            self.outcome(gate)?;
        }
        self.candidate_identities()?;
        let mut completed = Vec::with_capacity(RequiredGate::ALL.len());
        for (gate, slot) in RequiredGate::ALL.into_iter().zip(self.slots) {
            completed.push(slot.ok_or(CandidateCheckError::Missing(gate))?);
        }
        let outcomes = completed
            .try_into()
            .map_err(|_| CandidateCheckError::Inventory)?;
        Ok(CompletedCandidateChecks {
            captured: self.captured,
            profile: self.profile,
            outcomes,
        })
    }
}

impl<'run, 'source: 'run> CompletedCandidateChecks<'run, 'source> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }

    pub const fn profile(&self) -> &CandidateProfile {
        &self.profile
    }

    pub fn outcomes(&self) -> impl ExactSizeIterator<Item = RunnerOutcome<'run, 'source>> + '_ {
        self.outcomes.iter().copied()
    }
}
