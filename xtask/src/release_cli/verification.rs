use super::{artifact, gate_value, image, CliError, VerificationPort};
use std::io::{self, Write};
use xtask::{
    browser::BrowserOutcome,
    core_miri::CoreMiriOutcome,
    core_mutation::CoreMutationOutcome,
    core_proofs::CoreProofOutcome,
    input_fuzz::InputFuzzOutcome,
    native_sanitizers::NativeOutcome,
    release_check::{
        CandidateCheckError, CandidateChecks, CandidateImages, CandidateProfile,
        CapturedReleaseSource, FastOutcome, LinuxOciTarget, OciOutcome, ReferenceOutcome,
        ReleaseInputs, RequiredGate, RunnerOutcome, SdkOutcome, SecurityOutcome, ServerOutcome,
    },
    server_distribution::ServerTarget,
};

pub(super) struct NativeVerification<'source> {
    captured: &'source CapturedReleaseSource,
    inputs: &'source ReleaseInputs,
    reference: Option<ReferenceOutcome<'source>>,
    browser: Option<BrowserOutcome>,
    proofs: Option<CoreProofOutcome>,
    miri: Option<CoreMiriOutcome>,
    mutation: Option<CoreMutationOutcome>,
    fuzz: Option<InputFuzzOutcome>,
    sanitizers: Option<NativeOutcome>,
    sdk: Option<SdkOutcome<'source>>,
    servers: Vec<ServerOutcome<'source>>,
    oci: Vec<OciOutcome<'source>>,
    fast: Option<FastOutcome<'source>>,
    security: Option<SecurityOutcome<'source>>,
}

fn profile(inputs: &ReleaseInputs) -> Result<CandidateProfile, CliError> {
    CandidateProfile::new(
        CandidateImages {
            reference: inputs.reference_image(),
            browser: image(inputs, RequiredGate::Browser)?,
            core_proofs: image(inputs, RequiredGate::CoreProofs)?,
            core_miri: image(inputs, RequiredGate::CoreMiri)?,
            core_mutation: image(inputs, RequiredGate::CoreMutation)?,
            input_fuzz: image(inputs, RequiredGate::InputFuzz)?,
            native_sanitizers: image(inputs, RequiredGate::NativeSanitizers)?,
        },
        inputs.profile().fuzz_architecture(),
    )
    .map_err(CliError::Candidate)
}

fn outcome<T>(slot: &Option<T>, gate: RequiredGate) -> Result<&T, CliError> {
    slot.as_ref()
        .ok_or(CliError::Candidate(CandidateCheckError::Missing(gate)))
}

impl<'source> NativeVerification<'source> {
    pub(super) fn new(
        captured: &'source CapturedReleaseSource,
        inputs: &'source ReleaseInputs,
    ) -> Self {
        Self {
            captured,
            inputs,
            reference: None,
            browser: None,
            proofs: None,
            miri: None,
            mutation: None,
            fuzz: None,
            sanitizers: None,
            sdk: None,
            servers: Vec::with_capacity(ServerTarget::ALL.len()),
            oci: Vec::with_capacity(2),
            fast: None,
            security: None,
        }
    }

    fn read_single(&mut self, gate: RequiredGate) -> Result<(), CliError> {
        let directory = artifact(self.inputs, gate)?;
        match gate {
            RequiredGate::Reference => {
                self.reference = Some(gate_value(
                    gate,
                    self.captured
                        .read_reference_artifact(directory, self.inputs.reference_image()),
                )?)
            }
            RequiredGate::Browser => {
                self.browser = Some(gate_value(
                    gate,
                    self.captured
                        .read_browser_artifact(directory, image(self.inputs, gate)?),
                )?)
            }
            RequiredGate::CoreProofs => {
                self.proofs = Some(gate_value(
                    gate,
                    self.captured
                        .read_core_proofs_artifact(directory, image(self.inputs, gate)?),
                )?)
            }
            RequiredGate::CoreMiri => {
                self.miri = Some(gate_value(
                    gate,
                    self.captured
                        .read_core_miri_artifact(directory, image(self.inputs, gate)?),
                )?)
            }
            RequiredGate::CoreMutation => {
                self.mutation = Some(gate_value(
                    gate,
                    self.captured
                        .read_core_mutation_artifact(directory, image(self.inputs, gate)?),
                )?)
            }
            RequiredGate::InputFuzz => {
                self.fuzz = Some(gate_value(
                    gate,
                    self.captured.read_input_fuzz_artifact(
                        directory,
                        image(self.inputs, gate)?,
                        self.inputs.profile().fuzz_architecture(),
                    ),
                )?)
            }
            RequiredGate::NativeSanitizers => {
                self.sanitizers = Some(gate_value(
                    gate,
                    self.captured
                        .read_native_sanitizers_artifact(directory, image(self.inputs, gate)?),
                )?)
            }
            RequiredGate::Sdk => {
                self.sdk = Some(gate_value(
                    gate,
                    self.captured
                        .read_sdk_artifact(directory, self.inputs.sdk_candidate()),
                )?)
            }
            RequiredGate::Fast
            | RequiredGate::Security
            | RequiredGate::Server(_)
            | RequiredGate::Oci(_) => {
                return Err(CliError::Candidate(CandidateCheckError::Inventory));
            }
        }
        Ok(())
    }

    fn read_server(&mut self, target: ServerTarget) -> Result<(), CliError> {
        if self.servers.len() >= ServerTarget::ALL.len() {
            return Err(CliError::Candidate(CandidateCheckError::Inventory));
        }
        let gate = RequiredGate::Server(target);
        let checked = gate_value(
            gate,
            self.captured.read_server_artifact(
                artifact(self.inputs, gate)?,
                self.inputs.server_candidate(target),
                target,
                self.inputs.sdk_candidate(),
                self.inputs.server_execution(target),
            ),
        )?;
        self.servers.push(checked);
        Ok(())
    }

    fn read_oci(&mut self, target: LinuxOciTarget) -> Result<(), CliError> {
        if self.oci.len() >= 2 {
            return Err(CliError::Candidate(CandidateCheckError::Inventory));
        }
        let gate = RequiredGate::Oci(target);
        let checked = gate_value(
            gate,
            self.captured.read_oci_artifact(
                artifact(self.inputs, gate)?,
                self.inputs.server_candidate(target.server_target()),
                target,
                self.inputs.oci_archive(target),
                self.inputs.sdk_candidate(),
            ),
        )?;
        self.oci.push(checked);
        Ok(())
    }

    fn include_retained<'run>(
        &'run self,
        checks: &mut CandidateChecks<'run, 'source>,
    ) -> Result<(), CliError>
    where
        'source: 'run,
    {
        for checked in [
            RunnerOutcome::Reference(outcome(&self.reference, RequiredGate::Reference)?),
            RunnerOutcome::Browser(outcome(&self.browser, RequiredGate::Browser)?),
            RunnerOutcome::CoreProofs(outcome(&self.proofs, RequiredGate::CoreProofs)?),
            RunnerOutcome::CoreMiri(outcome(&self.miri, RequiredGate::CoreMiri)?),
            RunnerOutcome::CoreMutation(outcome(&self.mutation, RequiredGate::CoreMutation)?),
            RunnerOutcome::InputFuzz(outcome(&self.fuzz, RequiredGate::InputFuzz)?),
            RunnerOutcome::NativeSanitizers(outcome(
                &self.sanitizers,
                RequiredGate::NativeSanitizers,
            )?),
            RunnerOutcome::Sdk(outcome(&self.sdk, RequiredGate::Sdk)?),
        ] {
            checks.include(checked).map_err(CliError::Candidate)?;
        }
        for server in &self.servers {
            checks
                .include(RunnerOutcome::Server(server))
                .map_err(CliError::Candidate)?;
        }
        for oci in &self.oci {
            checks
                .include(RunnerOutcome::Oci(oci))
                .map_err(CliError::Candidate)?;
        }
        Ok(())
    }
}

impl VerificationPort for NativeVerification<'_> {
    type Error = CliError;

    fn read(&mut self, gate: RequiredGate) -> Result<(), Self::Error> {
        match gate {
            RequiredGate::Server(target) => self.read_server(target),
            RequiredGate::Oci(target) => self.read_oci(target),
            RequiredGate::Reference
            | RequiredGate::Browser
            | RequiredGate::CoreProofs
            | RequiredGate::CoreMiri
            | RequiredGate::CoreMutation
            | RequiredGate::InputFuzz
            | RequiredGate::NativeSanitizers
            | RequiredGate::Sdk => self.read_single(gate),
            RequiredGate::Fast | RequiredGate::Security => {
                Err(CliError::Candidate(CandidateCheckError::Inventory))
            }
        }
    }

    fn fresh(&mut self, gate: RequiredGate) -> Result<(), Self::Error> {
        match gate {
            RequiredGate::Fast => self.fast = Some(gate_value(gate, self.captured.verify_fast())?),
            RequiredGate::Security => {
                self.security = Some(gate_value(gate, self.captured.verify_security())?)
            }
            RequiredGate::Reference
            | RequiredGate::Browser
            | RequiredGate::CoreProofs
            | RequiredGate::CoreMiri
            | RequiredGate::CoreMutation
            | RequiredGate::InputFuzz
            | RequiredGate::NativeSanitizers
            | RequiredGate::Sdk
            | RequiredGate::Server(_)
            | RequiredGate::Oci(_) => {
                return Err(CliError::Candidate(CandidateCheckError::Inventory))
            }
        }
        Ok(())
    }

    fn complete(self) -> Result<(), Self::Error> {
        let mut checks = CandidateChecks::new(self.captured, profile(self.inputs)?)
            .map_err(CliError::Candidate)?;
        self.include_retained(&mut checks)?;
        checks
            .include(RunnerOutcome::Fast(outcome(
                &self.fast,
                RequiredGate::Fast,
            )?))
            .map_err(CliError::Candidate)?;
        checks
            .include(RunnerOutcome::Security(outcome(
                &self.security,
                RequiredGate::Security,
            )?))
            .map_err(CliError::Candidate)?;
        let completed = checks.finish().map_err(CliError::Candidate)?;
        let count = completed.outcomes().len();
        writeln!(
            io::stdout(),
            "release-check completed: {count} required gates"
        )
        .map_err(CliError::Output)
    }
}
