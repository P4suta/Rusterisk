use super::CandidateCheckError;
use crate::release_check::RequiredGate;
use crate::{
    input_fuzz::LinuxTarget,
    reference::{ImageIdentity, ReferenceImage},
};

pub struct CandidateImages<'a> {
    pub reference: &'a ReferenceImage,
    pub browser: &'a str,
    pub core_proofs: &'a str,
    pub core_miri: &'a str,
    pub core_mutation: &'a str,
    pub input_fuzz: &'a str,
    pub native_sanitizers: &'a str,
}

pub struct CandidateProfile {
    reference: ImageIdentity,
    browser: ImageIdentity,
    core_proofs: ImageIdentity,
    core_miri: ImageIdentity,
    core_mutation: ImageIdentity,
    input_fuzz: ImageIdentity,
    native_sanitizers: ImageIdentity,
    architecture: LinuxTarget,
}

fn image(value: &str, gate: RequiredGate) -> Result<ImageIdentity, CandidateCheckError> {
    ImageIdentity::parse(value).ok_or(CandidateCheckError::Profile(gate))
}

impl CandidateProfile {
    pub fn new(
        images: CandidateImages<'_>,
        architecture: LinuxTarget,
    ) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            reference: image(images.reference.as_str(), RequiredGate::Reference)?,
            browser: image(images.browser, RequiredGate::Browser)?,
            core_proofs: image(images.core_proofs, RequiredGate::CoreProofs)?,
            core_miri: image(images.core_miri, RequiredGate::CoreMiri)?,
            core_mutation: image(images.core_mutation, RequiredGate::CoreMutation)?,
            input_fuzz: image(images.input_fuzz, RequiredGate::InputFuzz)?,
            native_sanitizers: image(images.native_sanitizers, RequiredGate::NativeSanitizers)?,
            architecture,
        })
    }

    pub fn image(&self, gate: RequiredGate) -> Option<&str> {
        match gate {
            RequiredGate::Reference => Some(self.reference.as_str()),
            RequiredGate::Browser => Some(self.browser.as_str()),
            RequiredGate::CoreProofs => Some(self.core_proofs.as_str()),
            RequiredGate::CoreMiri => Some(self.core_miri.as_str()),
            RequiredGate::CoreMutation => Some(self.core_mutation.as_str()),
            RequiredGate::InputFuzz => Some(self.input_fuzz.as_str()),
            RequiredGate::NativeSanitizers => Some(self.native_sanitizers.as_str()),
            RequiredGate::Fast
            | RequiredGate::Security
            | RequiredGate::Sdk
            | RequiredGate::Server(_)
            | RequiredGate::Oci(_) => None,
        }
    }

    pub const fn fuzz_architecture(&self) -> LinuxTarget {
        self.architecture
    }
}
