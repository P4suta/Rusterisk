use crate::{
    browser::BrowserOutcome,
    core_miri::CoreMiriOutcome,
    core_mutation::CoreMutationOutcome,
    core_proofs::CoreProofOutcome,
    input_fuzz::InputFuzzOutcome,
    native_sanitizers::NativeOutcome,
    release_check::{
        FastOutcome, InputError, OciOutcome, ReferenceOutcome, RequiredGate, SdkOutcome,
        SecurityOutcome, ServerOutcome, SourceInputs,
    },
};

#[derive(Clone, Copy)]
pub enum RunnerOutcome<'run, 'source: 'run> {
    Fast(&'run FastOutcome<'source>),
    Security(&'run SecurityOutcome<'source>),
    Reference(&'run ReferenceOutcome<'source>),
    Browser(&'run BrowserOutcome),
    CoreProofs(&'run CoreProofOutcome),
    CoreMiri(&'run CoreMiriOutcome),
    CoreMutation(&'run CoreMutationOutcome),
    InputFuzz(&'run InputFuzzOutcome),
    NativeSanitizers(&'run NativeOutcome),
    Sdk(&'run SdkOutcome<'source>),
    Server(&'run ServerOutcome<'source>),
    Oci(&'run OciOutcome<'source>),
}

impl<'run, 'source: 'run> RunnerOutcome<'run, 'source> {
    pub const fn gate(self) -> RequiredGate {
        match self {
            Self::Fast(_) => RequiredGate::Fast,
            Self::Security(_) => RequiredGate::Security,
            Self::Reference(_) => RequiredGate::Reference,
            Self::Browser(_) => RequiredGate::Browser,
            Self::CoreProofs(_) => RequiredGate::CoreProofs,
            Self::CoreMiri(_) => RequiredGate::CoreMiri,
            Self::CoreMutation(_) => RequiredGate::CoreMutation,
            Self::InputFuzz(_) => RequiredGate::InputFuzz,
            Self::NativeSanitizers(_) => RequiredGate::NativeSanitizers,
            Self::Sdk(_) => RequiredGate::Sdk,
            Self::Server(outcome) => RequiredGate::Server(outcome.target()),
            Self::Oci(outcome) => RequiredGate::Oci(outcome.target()),
        }
    }

    pub(super) fn inputs(self) -> Result<SourceInputs<'run>, InputError> {
        match self {
            Self::Fast(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Security(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Reference(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Browser(outcome) => SourceInputs::new(outcome.inputs()),
            Self::CoreProofs(outcome) => SourceInputs::new(outcome.inputs()),
            Self::CoreMiri(outcome) => SourceInputs::new(outcome.inputs()),
            Self::CoreMutation(outcome) => SourceInputs::new(outcome.inputs()),
            Self::InputFuzz(outcome) => SourceInputs::new(outcome.inputs()),
            Self::NativeSanitizers(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Sdk(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Server(outcome) => SourceInputs::new(outcome.inputs()),
            Self::Oci(outcome) => SourceInputs::new(outcome.inputs()),
        }
    }

    pub(super) fn image(self) -> Option<&'run str> {
        match self {
            Self::Reference(outcome) => Some(outcome.image().as_str()),
            Self::Browser(outcome) => Some(outcome.image()),
            Self::CoreProofs(outcome) => Some(outcome.image()),
            Self::CoreMiri(outcome) => Some(outcome.image()),
            Self::CoreMutation(outcome) => Some(outcome.image()),
            Self::InputFuzz(outcome) => Some(outcome.image()),
            Self::NativeSanitizers(outcome) => Some(outcome.image()),
            Self::Fast(_) | Self::Security(_) | Self::Sdk(_) | Self::Server(_) | Self::Oci(_) => {
                None
            }
        }
    }

    pub(super) const fn inputs_are_release_source(self) -> bool {
        match self {
            Self::Fast(_)
            | Self::Security(_)
            | Self::Reference(_)
            | Self::Sdk(_)
            | Self::Server(_)
            | Self::Oci(_) => true,
            Self::Browser(_)
            | Self::CoreProofs(_)
            | Self::CoreMiri(_)
            | Self::CoreMutation(_)
            | Self::InputFuzz(_)
            | Self::NativeSanitizers(_) => false,
        }
    }
}
