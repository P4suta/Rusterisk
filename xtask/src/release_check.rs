use crate::server_distribution::ServerTarget;

mod aggregate;
#[cfg(all(test, unix))]
mod candidate_fixtures;
mod candidate_run;
mod inputs;
mod native;
#[cfg(unix)]
mod native_artifacts;
#[cfg(all(test, unix))]
mod native_artifacts_tests;
mod oci;
mod reference;
mod sdk;
mod server;
mod source;
pub use aggregate::{
    CandidateCheckError, CandidateChecks, CandidateImages, CandidateProfile,
    CompletedCandidateChecks, RunnerOutcome,
};
pub use inputs::{InputError, InputKey, SourceInputs};
pub use native::{FastOutcome, NativeGateError, SecurityOutcome};
pub use oci::{OciOutcome, OciRunError};
pub use reference::{ReferenceOutcome, ReferenceRunError};
pub use sdk::{SdkOutcome, SdkRunError};
pub use server::{ServerOutcome, ServerRunError};
pub use source::{CapturedReleaseSource, ReleaseSourceError};

#[cfg(any(test, doctest))]
#[path = "release_check/aggregate/contracts.rs"]
mod aggregate_contracts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxOciTarget {
    X86_64,
    Arm64,
}

impl LinuxOciTarget {
    pub const fn server_target(self) -> ServerTarget {
        match self {
            Self::X86_64 => ServerTarget::LinuxX86_64,
            Self::Arm64 => ServerTarget::LinuxArm64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredGate {
    Fast,
    Security,
    Reference,
    Browser,
    CoreProofs,
    CoreMiri,
    CoreMutation,
    InputFuzz,
    NativeSanitizers,
    Sdk,
    Server(ServerTarget),
    Oci(LinuxOciTarget),
}

impl RequiredGate {
    pub const ALL: [Self; 17] = [
        Self::Fast,
        Self::Security,
        Self::Reference,
        Self::Browser,
        Self::CoreProofs,
        Self::CoreMiri,
        Self::CoreMutation,
        Self::InputFuzz,
        Self::NativeSanitizers,
        Self::Sdk,
        Self::Server(ServerTarget::LinuxX86_64),
        Self::Server(ServerTarget::LinuxArm64),
        Self::Server(ServerTarget::MacOsArm64),
        Self::Server(ServerTarget::MacOsX86_64),
        Self::Server(ServerTarget::WindowsX86_64),
        Self::Oci(LinuxOciTarget::X86_64),
        Self::Oci(LinuxOciTarget::Arm64),
    ];
}

impl std::fmt::Display for RequiredGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => formatter.write_str("fast"),
            Self::Security => formatter.write_str("security"),
            Self::Reference => formatter.write_str("reference"),
            Self::Browser => formatter.write_str("browser"),
            Self::CoreProofs => formatter.write_str("core-proofs"),
            Self::CoreMiri => formatter.write_str("core-miri"),
            Self::CoreMutation => formatter.write_str("core-mutation"),
            Self::InputFuzz => formatter.write_str("input-fuzz"),
            Self::NativeSanitizers => formatter.write_str("native-sanitizers"),
            Self::Sdk => formatter.write_str("sdk"),
            Self::Server(target) => write!(formatter, "server:{}", target.triple()),
            Self::Oci(target) => write!(formatter, "oci:{}", target.server_target().triple()),
        }
    }
}
