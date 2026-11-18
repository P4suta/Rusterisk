use super::TaskError;
use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use xtask::{
    release_check::{
        CandidateCheckError, CapturedReleaseSource, LinuxOciTarget, ReleaseInputError,
        ReleaseInputs, ReleaseRequest, RequiredGate,
    },
    server_distribution::ServerTarget,
};

const MAX_INPUT_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests;
mod verification;
use verification::NativeVerification;

enum InputFailure {
    Io(io::Error),
    RegularFile,
    Changed,
    Parse(ReleaseInputError),
}

impl fmt::Display for InputFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::RegularFile => formatter.write_str("release input file must be a regular file"),
            Self::Changed => formatter.write_str("release input file changed while reading"),
            Self::Parse(error) => error.fmt(formatter),
        }
    }
}

impl From<io::Error> for InputFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

enum CliError {
    Input {
        path: PathBuf,
        failure: InputFailure,
    },
    MissingArtifact(RequiredGate),
    Candidate(CandidateCheckError),
    Source(String),
    Gate {
        gate: RequiredGate,
        diagnostic: String,
    },
    Output(io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input { path, failure } => {
                write!(
                    formatter,
                    "release-check input {}: {failure}",
                    path.display()
                )
            }
            Self::MissingArtifact(gate) => {
                write!(formatter, "release-check artifact input is missing: {gate}")
            }
            Self::Candidate(error) => write!(formatter, "release-check candidate: {error}"),
            Self::Source(diagnostic) => {
                write!(formatter, "release-check captured source: {diagnostic}")
            }
            Self::Gate { gate, diagnostic } => {
                write!(formatter, "release-check {gate}: {diagnostic}")
            }
            Self::Output(error) => error.fmt(formatter),
        }
    }
}

enum RetainedGate {
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

enum GateCapability {
    FreshFast,
    FreshSecurity,
    Retained(RetainedGate),
}

fn capability(gate: RequiredGate) -> GateCapability {
    match gate {
        RequiredGate::Fast => GateCapability::FreshFast,
        RequiredGate::Security => GateCapability::FreshSecurity,
        RequiredGate::Sdk => GateCapability::Retained(RetainedGate::Sdk),
        RequiredGate::Server(target) => GateCapability::Retained(RetainedGate::Server(target)),
        RequiredGate::Oci(target) => GateCapability::Retained(RetainedGate::Oci(target)),
        RequiredGate::Reference => GateCapability::Retained(RetainedGate::Reference),
        RequiredGate::Browser => GateCapability::Retained(RetainedGate::Browser),
        RequiredGate::CoreProofs => GateCapability::Retained(RetainedGate::CoreProofs),
        RequiredGate::CoreMiri => GateCapability::Retained(RetainedGate::CoreMiri),
        RequiredGate::CoreMutation => GateCapability::Retained(RetainedGate::CoreMutation),
        RequiredGate::InputFuzz => GateCapability::Retained(RetainedGate::InputFuzz),
        RequiredGate::NativeSanitizers => GateCapability::Retained(RetainedGate::NativeSanitizers),
    }
}

fn read_inputs(path: &Path) -> Result<ReleaseInputs, InputFailure> {
    let name = path.file_name().ok_or(InputFailure::RegularFile)?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        Some(_) => Path::new("."),
        None => return Err(InputFailure::RegularFile),
    };
    let path = fs::canonicalize(parent)?.join(name);
    let before = fs::symlink_metadata(&path)?;
    if !before.is_file() {
        return Err(InputFailure::RegularFile);
    }
    if before.len() > MAX_INPUT_BYTES as u64 {
        return Err(InputFailure::Parse(ReleaseInputError::Limit));
    }
    let mut file = fs::File::open(&path)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(InputFailure::RegularFile);
    }
    if opened.len() != before.len() || opened.modified()? != before.modified()? {
        return Err(InputFailure::Changed);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(InputFailure::Parse(ReleaseInputError::Limit));
    }
    let after = file.metadata()?;
    if bytes.len() as u64 != opened.len()
        || after.len() != opened.len()
        || after.modified()? != opened.modified()?
    {
        return Err(InputFailure::Changed);
    }
    ReleaseInputs::parse(&bytes, &path).map_err(InputFailure::Parse)
}

fn load(path: PathBuf) -> Result<ReleaseInputs, CliError> {
    read_inputs(&path).map_err(|failure| CliError::Input { path, failure })
}

fn artifact(inputs: &ReleaseInputs, gate: RequiredGate) -> Result<&Path, CliError> {
    inputs.artifact(gate).ok_or(CliError::MissingArtifact(gate))
}

fn image(inputs: &ReleaseInputs, gate: RequiredGate) -> Result<&str, CliError> {
    inputs
        .profile()
        .image(gate)
        .ok_or(CliError::Candidate(CandidateCheckError::Profile(gate)))
}

fn gate_result<T>(
    gate: RequiredGate,
    result: Result<T, impl fmt::Display>,
) -> Result<(), CliError> {
    gate_value(gate, result).map(|_| ())
}

fn gate_value<T>(gate: RequiredGate, result: Result<T, impl fmt::Display>) -> Result<T, CliError> {
    result.map_err(|error| CliError::Gate {
        gate,
        diagnostic: error.to_string(),
    })
}

fn run(inputs: &ReleaseInputs, gate: RequiredGate) -> Result<(), CliError> {
    let capability = capability(gate);
    let captured = CapturedReleaseSource::read(inputs.source())
        .map_err(|error| CliError::Source(error.to_string()))?;
    match capability {
        GateCapability::FreshFast => gate_result(gate, captured.verify_fast())?,
        GateCapability::FreshSecurity => gate_result(gate, captured.verify_security())?,
        GateCapability::Retained(RetainedGate::Reference) => gate_result(
            gate,
            captured.verify_reference_artifact(inputs.reference_image(), artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::Browser) => gate_result(
            gate,
            captured.verify_browser_artifact(image(inputs, gate)?, artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::CoreProofs) => gate_result(
            gate,
            captured.verify_core_proofs_artifact(image(inputs, gate)?, artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::CoreMiri) => gate_result(
            gate,
            captured.verify_core_miri_artifact(image(inputs, gate)?, artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::CoreMutation) => gate_result(
            gate,
            captured.verify_core_mutation_artifact(image(inputs, gate)?, artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::InputFuzz) => gate_result(
            gate,
            captured.verify_input_fuzz_artifact(
                image(inputs, gate)?,
                inputs.profile().fuzz_architecture(),
                artifact(inputs, gate)?,
            ),
        )?,
        GateCapability::Retained(RetainedGate::NativeSanitizers) => gate_result(
            gate,
            captured
                .verify_native_sanitizers_artifact(image(inputs, gate)?, artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::Sdk) => gate_result(
            gate,
            captured.verify_sdk_artifact(inputs.sdk_candidate(), artifact(inputs, gate)?),
        )?,
        GateCapability::Retained(RetainedGate::Server(target)) => gate_result(
            gate,
            captured.verify_server_artifact(
                inputs.server_candidate(target),
                target,
                inputs.sdk_candidate(),
                inputs.server_execution(target),
                artifact(inputs, gate)?,
            ),
        )?,
        GateCapability::Retained(RetainedGate::Oci(target)) => gate_result(
            gate,
            captured.verify_oci_artifact(
                inputs.server_candidate(target.server_target()),
                target,
                inputs.oci_archive(target),
                inputs.sdk_candidate(),
                artifact(inputs, gate)?,
            ),
        )?,
    }
    writeln!(io::stdout(), "release-check completed: {gate}").map_err(CliError::Output)
}

trait VerificationPort {
    type Error;
    fn read(&mut self, gate: RequiredGate) -> Result<(), Self::Error>;
    fn fresh(&mut self, gate: RequiredGate) -> Result<(), Self::Error>;
    fn complete(self) -> Result<(), Self::Error>;
}

fn verify_with<P: VerificationPort>(mut port: P) -> Result<(), P::Error> {
    for gate in RequiredGate::ALL {
        if !matches!(gate, RequiredGate::Fast | RequiredGate::Security) {
            port.read(gate)?;
        }
    }
    port.fresh(RequiredGate::Fast)?;
    port.fresh(RequiredGate::Security)?;
    port.complete()
}

fn verify(inputs: &ReleaseInputs) -> Result<(), CliError> {
    let captured = CapturedReleaseSource::read(inputs.source())
        .map_err(|error| CliError::Source(error.to_string()))?;
    verify_with(NativeVerification::new(&captured, inputs))
}

pub(super) fn execute(arguments: &[String]) -> Result<(), TaskError> {
    let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
    let request = ReleaseRequest::parse(&arguments)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let result = match request {
        ReleaseRequest::Inventory => {
            let mut output = io::stdout();
            writeln!(
                output,
                "release-check plan: {} required gates",
                RequiredGate::ALL.len()
            )?;
            for gate in RequiredGate::ALL {
                writeln!(output, "required: {gate}")?;
            }
            Ok(())
        }
        ReleaseRequest::Run { gate, inputs } => load(inputs).and_then(|inputs| run(&inputs, gate)),
        ReleaseRequest::Verify { inputs } => load(inputs).and_then(|inputs| verify(&inputs)),
    };
    result.map_err(|error| TaskError::Contract(error.to_string()))
}
