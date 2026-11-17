use super::{index, CandidateCheckError};
use crate::release_check::{CapturedReleaseSource, InputKey, RequiredGate, SourceInputs};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) struct ProjectionSet {
    keys: [Option<InputKey>; RequiredGate::ALL.len()],
}

impl ProjectionSet {
    pub(super) const fn new() -> Self {
        Self {
            keys: [None; RequiredGate::ALL.len()],
        }
    }

    pub(super) fn expected(
        &mut self,
        captured: &CapturedReleaseSource,
        gate: RequiredGate,
    ) -> Result<InputKey, CandidateCheckError> {
        let position = index(gate)?;
        if let Some(key) = self.keys.get(position).copied().flatten() {
            return Ok(key);
        }
        let key = match gate {
            RequiredGate::Fast | RequiredGate::Security | RequiredGate::Reference => captured
                .inputs()
                .map_err(|error| CandidateCheckError::Inputs { gate, error })?
                .key(),
            RequiredGate::Browser
            | RequiredGate::CoreProofs
            | RequiredGate::CoreMiri
            | RequiredGate::CoreMutation
            | RequiredGate::InputFuzz
            | RequiredGate::NativeSanitizers
            | RequiredGate::Sdk
            | RequiredGate::Server(_)
            | RequiredGate::Oci(_) => {
                let copy = ProjectionCopy::create()?;
                let operation = copy.project(captured, gate);
                let cleanup = copy.close();
                conclude(operation, cleanup)?
            }
        };
        *self
            .keys
            .get_mut(position)
            .ok_or(CandidateCheckError::Inventory)? = Some(key);
        Ok(key)
    }
}

fn conclude(
    operation: Result<InputKey, CandidateCheckError>,
    cleanup: Result<(), CandidateCheckError>,
) -> Result<InputKey, CandidateCheckError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(CandidateCheckError::Source {
            stage: "projection cleanup",
            cause: format!("{primary}; {cleanup}"),
        }),
    }
}

fn error(stage: &'static str, cause: impl std::fmt::Display) -> CandidateCheckError {
    CandidateCheckError::Source {
        stage,
        cause: cause.to_string(),
    }
}

struct ProjectionCopy {
    directory: PathBuf,
}

impl ProjectionCopy {
    fn create() -> Result<Self, CandidateCheckError> {
        let parent = fs::canonicalize(std::env::temp_dir())
            .map_err(|cause| error("projection temporary root", cause))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|cause| error("projection clock", cause))?
            .as_nanos();
        for attempt in 0..32 {
            let directory = parent.join(format!(
                "rusterisk-release-projection-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => return Ok(Self { directory }),
                Err(cause) if cause.kind() == io::ErrorKind::AlreadyExists => {}
                Err(cause) => return Err(error("projection reservation", cause)),
            }
        }
        Err(error(
            "projection reservation",
            "temporary directory reservation limit exceeded",
        ))
    }

    fn project(
        &self,
        captured: &CapturedReleaseSource,
        gate: RequiredGate,
    ) -> Result<InputKey, CandidateCheckError> {
        let source = self.directory.join("source");
        captured
            .write(&source, &self.directory.join("state"))
            .map_err(|cause| error("projection source copy", cause))?;
        captured
            .check(&source)
            .map_err(|cause| error("projection captured inputs", cause))?;
        let key = expected(captured, &source, gate)?;
        captured
            .check(&source)
            .map_err(|cause| error("projection inputs after capture", cause))?;
        Ok(key)
    }

    fn close(self) -> Result<(), CandidateCheckError> {
        let metadata = fs::symlink_metadata(&self.directory)
            .map_err(|cause| error("projection owned directory", cause))?;
        if !metadata.is_dir()
            || fs::canonicalize(&self.directory)
                .map_err(|cause| error("projection owned path", cause))?
                != self.directory
        {
            return Err(error(
                "projection owned directory",
                "temporary projection directory identity changed",
            ));
        }
        fs::remove_dir_all(&self.directory)
            .map_err(|cause| error("projection owned cleanup", cause))
    }
}

fn bind<'p, 'b>(
    captured: &CapturedReleaseSource,
    files: impl Iterator<Item = (&'p Path, &'b [u8])>,
    gate: RequiredGate,
) -> Result<InputKey, CandidateCheckError> {
    let projection =
        SourceInputs::new(files).map_err(|error| CandidateCheckError::Inputs { gate, error })?;
    captured
        .inputs()
        .and_then(|source| source.bind(&projection))
        .map_err(|error| CandidateCheckError::Inputs { gate, error })
}

fn expected(
    captured: &CapturedReleaseSource,
    source: &Path,
    gate: RequiredGate,
) -> Result<InputKey, CandidateCheckError> {
    match gate {
        RequiredGate::Browser => {
            let capture = crate::browser::SourceSnapshot::read(source)
                .map_err(|cause| error("browser projection", cause))?;
            bind(captured, capture.files(), gate)
        }
        RequiredGate::CoreProofs => {
            let capture = crate::core_proofs::CapturedCoreProofs::read(source)
                .map_err(|cause| error("proof projection", cause))?;
            bind(captured, capture.inputs(), gate)
        }
        RequiredGate::CoreMiri => {
            let capture = crate::core_miri::CapturedCoreMiri::read(source)
                .map_err(|cause| error("Miri projection", cause))?;
            bind(captured, capture.inputs(), gate)
        }
        RequiredGate::CoreMutation => {
            let capture = crate::core_mutation::CapturedCoreMutation::read(source)
                .map_err(|cause| error("mutation projection", cause))?;
            bind(captured, capture.inputs(), gate)
        }
        RequiredGate::InputFuzz => {
            let capture = crate::input_fuzz::CapturedInputFuzz::read(source)
                .map_err(|cause| error("fuzz projection", cause))?;
            bind(captured, capture.inputs(), gate)
        }
        RequiredGate::NativeSanitizers => {
            let capture = crate::native_sanitizers::CapturedNative::read(source)
                .map_err(|cause| error("native sanitizer projection", cause))?;
            bind(captured, capture.inputs(), gate)
        }
        RequiredGate::Sdk => {
            let capture = crate::distribution::CapturedSdk::read(source)
                .map_err(|cause| error("SDK projection", cause))?;
            bind(captured, capture.files(), gate)
        }
        RequiredGate::Server(target) => server(captured, source, target, gate),
        RequiredGate::Oci(target) => server(captured, source, target.server_target(), gate),
        RequiredGate::Fast | RequiredGate::Security | RequiredGate::Reference => {
            Err(CandidateCheckError::Inventory)
        }
    }
}

fn server(
    captured: &CapturedReleaseSource,
    source: &Path,
    target: crate::server_distribution::ServerTarget,
    gate: RequiredGate,
) -> Result<InputKey, CandidateCheckError> {
    let capture = crate::server_distribution::CapturedServer::read(source, target)
        .map_err(|cause| error("server projection", cause))?;
    bind(captured, capture.files(), gate)
}
