use crate::{
    core_proofs::{self, CapturedCoreProofs, CoreProofOutcome, CoreProofPlan, ProofError},
    process::{CommandSupervisor, ProcessError},
    release_check::{
        native_artifacts::{
            fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
            PublishedNativeArtifact,
        },
        CapturedReleaseSource, ReleaseSourceError, SourceInputs,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const METADATA: &str = "native-check.toml";
const FORMAT: u32 = 1;

#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod lifecycle_tests;

trait ProofPort {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError>;
    fn run_cleanup(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl ProofPort for CommandSupervisor {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        CommandSupervisor::run(self, command, budget)
    }

    fn run_cleanup(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        CommandSupervisor::run_cleanup(self, command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        CommandSupervisor::finish(self)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    image: String,
    source_root: PathBuf,
    results_root: PathBuf,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> ProofError {
    ProofError::Io(io::Error::new(io::ErrorKind::InvalidData, reason))
}

fn source(error: ReleaseSourceError) -> ProofError {
    match error {
        ReleaseSourceError::Changed => ProofError::SourceChanged,
        ReleaseSourceError::Distribution(_)
        | ReleaseSourceError::Io(_)
        | ReleaseSourceError::Inputs(_)
        | ReleaseSourceError::Invalid => ProofError::Io(io::Error::other(error.to_string())),
    }
}

fn bind_capture(
    captured: &CapturedReleaseSource,
    actual: &CapturedCoreProofs,
) -> Result<(), ProofError> {
    let projection = SourceInputs::new(actual.inputs())
        .map_err(|error| ProofError::Io(io::Error::other(error)))?;
    captured
        .inputs()
        .and_then(|selected| selected.bind(&projection))
        .map(|_| ())
        .map_err(|_| ProofError::SourceChanged)
}

fn follow<T>(
    operation: Result<T, ProofError>,
    follow_up: Result<(), ProofError>,
    context: &'static str,
) -> Result<T, ProofError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(follow_up)) => Err(ProofError::FollowUp {
            operation: operation.to_string(),
            context,
            follow_up: follow_up.to_string(),
        }),
    }
}

fn original_path(path: &Path) -> Result<(), ProofError> {
    let text = path.to_str().ok_or(ProofError::Plan)?;
    if !path.is_absolute()
        || text.len() > 4096
        || text
            .chars()
            .any(|value| value.is_control() || value == '\\')
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || text.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid(
            "original proof paths require bounded absolute components",
        ));
    }
    Ok(())
}

fn original_plan(metadata: &Metadata, image: &str) -> Result<CoreProofPlan, ProofError> {
    if metadata.format != FORMAT || metadata.kind != "core-proofs" || metadata.image != image {
        return Err(invalid("retained proof format, gate, or image differs"));
    }
    original_path(&metadata.source_root)?;
    original_path(&metadata.results_root)?;
    let directory = metadata
        .source_root
        .ancestors()
        .nth(3)
        .ok_or(ProofError::Plan)?;
    let identity = directory
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_prefix("rusterisk-core-proofs-artifact-"))
        .ok_or(ProofError::Plan)?;
    if metadata.source_root != directory.join("source/crates/core")
        || metadata.results_root != directory.join("results")
    {
        return Err(invalid(
            "original proof paths differ from the owned execution profile",
        ));
    }
    CoreProofPlan::new(
        identity,
        metadata.source_root.to_str().ok_or(ProofError::Plan)?,
        metadata.results_root.to_str().ok_or(ProofError::Plan)?,
        image,
    )
}

struct Workspace {
    root: PathBuf,
    identity: String,
}

impl Workspace {
    fn create() -> Result<Self, ProofError> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ProofError::Plan)?
            .as_nanos();
        for attempt in 0..32 {
            let identity = format!("{}-{stamp}-{attempt}", std::process::id());
            let root = parent.join(format!("rusterisk-core-proofs-artifact-{identity}"));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root, identity }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "retained proof reservation failed",
        )
        .into())
    }

    fn release_source(&self) -> PathBuf {
        self.root.join("release-source")
    }

    fn close(self) -> Result<(), ProofError> {
        if !fs::symlink_metadata(&self.root)?.is_dir() || fs::canonicalize(&self.root)? != self.root
        {
            return Err(invalid("retained proof workspace identity changed"));
        }
        fs::remove_dir_all(self.root).map_err(ProofError::Io)
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), ProofError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

struct Prepared<'a> {
    captured: &'a CapturedReleaseSource,
    image: &'a str,
    publication: NativeArtifactPublication,
    workspace: Workspace,
}

fn remove_staging(path: &Path) -> Result<(), ProofError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.is_dir() && fs::canonicalize(path)? == path => {
            fs::remove_dir_all(path).map_err(ProofError::Io)
        }
        Ok(_) => Err(invalid("retained proof staging identity changed")),
    }
}

fn prepare<'a>(
    captured: &'a CapturedReleaseSource,
    image: &'a str,
    destination: &Path,
) -> Result<Prepared<'a>, ProofError> {
    core_proofs::plan(image)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ProofError::Plan)?
        .as_nanos();
    let parent = destination.parent().ok_or(ProofError::Plan)?;
    let staging = parent.join(format!("proof-artifacts-{}-{stamp}", std::process::id()));
    let publication = NativeArtifactPublication::create(&staging, destination)?;
    let workspace = match Workspace::create() {
        Ok(workspace) => workspace,
        Err(error) => {
            return follow(
                Err(error),
                remove_staging(publication.staging()),
                "retained proof staging cleanup failed",
            );
        }
    };
    Ok(Prepared {
        captured,
        image,
        publication,
        workspace,
    })
}

fn publish(
    publication: NativeArtifactPublication,
    completed: Result<CoreProofOutcome, ProofError>,
) -> Result<CoreProofOutcome, ProofError> {
    let staging = publication.staging().to_owned();
    let operation = completed.and_then(|outcome| {
        publication
            .publish_completed(Ok(outcome))
            .map(|(_, outcome)| outcome)
            .map_err(ProofError::Io)
    });
    match operation {
        Ok(outcome) => Ok(outcome),
        Err(error) => follow(
            Err(error),
            remove_staging(&staging),
            "retained proof staging cleanup failed",
        ),
    }
}

fn execute_with(
    prepared: Prepared<'_>,
    mut port: impl ProofPort,
) -> Result<CoreProofOutcome, ProofError> {
    let Prepared {
        captured,
        image,
        publication,
        workspace,
    } = prepared;
    let operation = (|| {
        captured
            .write(
                &workspace.release_source(),
                &workspace.root.join("captured-browser"),
            )
            .map_err(source)?;
        captured
            .check(&workspace.release_source())
            .map_err(source)?;
        let proof = CapturedCoreProofs::read(&workspace.release_source())?;
        bind_capture(captured, &proof)?;
        core_proofs::verify_retained(
            proof,
            image,
            &workspace.identity,
            &workspace.root,
            |command, budget, mode| match mode {
                core_proofs::CommandMode::Work => port.run(command, budget),
                core_proofs::CommandMode::Cleanup => port.run_cleanup(command, budget),
            },
        )
    })();
    let finished = port.finish().map_err(ProofError::Process);
    let full_check = captured.check(&workspace.release_source()).map_err(source);
    let proof_check = match &operation {
        Ok(proof) => proof.check(),
        Err(_) => Ok(()),
    };
    let operation = follow(
        operation,
        finished,
        "retained proof supervisor cleanup failed",
    );
    let operation = follow(operation, full_check, "full proof source check failed");
    let operation = follow(operation, proof_check, "final retained proof check failed");
    let native = publication.staging().join("native");
    let operation = operation.and_then(|proof| {
        fs::create_dir(&native)?;
        captured
            .write(
                &native.join("release-source"),
                &native.join("captured-browser"),
            )
            .map_err(source)?;
        proof.preserve(&native)?;
        Ok(proof)
    });
    let operation = follow(
        operation,
        workspace.close(),
        "retained proof temporary cleanup failed",
    );
    let completed = operation.and_then(|proof| {
        let metadata = Metadata {
            format: FORMAT,
            kind: "core-proofs".to_owned(),
            image: image.to_owned(),
            source_root: proof.source_root().to_owned(),
            results_root: proof.results_root().to_owned(),
            files: fingerprints(publication.staging())?,
        };
        original_plan(&metadata, image)?;
        captured
            .check(&native.join("release-source"))
            .map_err(source)?;
        proof.check_preserved(&native)?;
        let bytes =
            toml::to_string(&metadata).map_err(|error| ProofError::Io(io::Error::other(error)))?;
        write(&publication.staging().join(METADATA), bytes.as_bytes())?;
        read(captured, publication.staging(), image)?;
        Ok(proof.into_outcome())
    });
    publish(publication, completed)
}

pub(super) fn produce(
    captured: &CapturedReleaseSource,
    image: &str,
    destination: &Path,
) -> Result<CoreProofOutcome, ProofError> {
    let prepared = prepare(captured, image, destination)?;
    match CommandSupervisor::new() {
        Ok(port) => execute_with(prepared, port),
        Err(error) => {
            let operation = follow(
                Err(ProofError::Process(error)),
                prepared.workspace.close(),
                "retained proof temporary cleanup failed",
            );
            publish(prepared.publication, operation)
        }
    }
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
) -> Result<CoreProofOutcome, ProofError> {
    core_proofs::plan(image)?;
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(
        std::str::from_utf8(&bytes).map_err(|error| ProofError::Io(io::Error::other(error)))?,
    )
    .map_err(|error| ProofError::Io(io::Error::other(error.to_string())))?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if metadata.files != current {
        return Err(invalid("retained proof bytes or inventory differ"));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let published = PublishedNativeArtifact::read(directory, &expected)?;
    let native = published.directory().join("native");
    let plan = original_plan(&metadata, image)?;
    captured
        .check(&native.join("release-source"))
        .map_err(source)?;
    let proof = CapturedCoreProofs::read(&native.join("release-source"))?;
    core_proofs::read_retained(
        proof,
        &native.join("core-source"),
        &native.join("results"),
        plan,
    )
}
