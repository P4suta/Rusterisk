use super::{read_bound_fuzz, CapturedReleaseSource, FuzzError, InputFuzzOutcome, LinuxTarget};
use crate::{
    input_fuzz::FuzzExecution,
    reference::ImageIdentity,
    release_check::native_artifacts::{
        fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
        PublishedNativeArtifact,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const METADATA: &str = "native-check.toml";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    image: String,
    architecture: String,
    execution: FuzzExecution,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> FuzzError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

fn profile(metadata: &Metadata, image: &str, architecture: LinuxTarget) -> Result<(), FuzzError> {
    if metadata.format != 1
        || metadata.kind != "input-fuzz"
        || metadata.image != image
        || metadata.architecture != architecture.triple()
    {
        return Err(invalid(
            "input fuzz artifact format, image, or target differs",
        ));
    }
    metadata.execution.check(image, architecture)
}

fn publication(destination: &Path) -> Result<NativeArtifactPublication, FuzzError> {
    let parent = destination.parent().ok_or(FuzzError::Plan)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let staging = parent.join(format!(
            "fuzz-artifact-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match NativeArtifactPublication::create(&staging, destination) {
            Ok(publication) => return Ok(publication),
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&staging).is_ok()
                    && fs::symlink_metadata(destination)
                        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "input fuzz artifact reservation limit exceeded",
    )
    .into())
}

fn check_inputs(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &str,
    architecture: LinuxTarget,
    execution: &FuzzExecution,
) -> Result<InputFuzzOutcome, FuzzError> {
    read_bound_fuzz(captured, &root.join("native/release-source"))?.read_retained(
        image,
        architecture,
        &root.join("native/fuzz"),
        execution,
    )
}

fn record(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &str,
    architecture: LinuxTarget,
    execution: FuzzExecution,
) -> Result<(), FuzzError> {
    check_inputs(captured, root, image, architecture, &execution)?;
    let metadata = Metadata {
        format: 1,
        kind: "input-fuzz".to_owned(),
        image: image.to_owned(),
        architecture: architecture.triple().to_owned(),
        execution,
        files: fingerprints(root)?,
    };
    profile(&metadata, image, architecture)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(METADATA))?;
    file.write_all(
        toml::to_string(&metadata)
            .map_err(io::Error::other)?
            .as_bytes(),
    )?;
    file.flush()?;
    Ok(())
}

fn execute(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &str,
    architecture: LinuxTarget,
) -> Result<InputFuzzOutcome, FuzzError> {
    fs::create_dir(root.join("native"))?;
    let source = root.join("native/release-source");
    captured
        .write(&source, &root.join("native/captured-browser"))
        .map_err(io::Error::other)?;
    let capture = read_bound_fuzz(captured, &source)?;
    let operation = capture.verify_retained(image, architecture, &root.join("native/fuzz"));
    let final_check = captured
        .check(&source)
        .map_err(io::Error::other)
        .map_err(FuzzError::Io);
    let (outcome, execution) = match (operation, final_check) {
        (operation, Ok(())) => operation?,
        (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(follow_up)) => {
            return Err(FuzzError::FollowUp {
                operation: operation.to_string(),
                context: "input fuzz final release source check failed",
                follow_up: follow_up.to_string(),
            })
        }
    };
    record(captured, root, image, architecture, execution)?;
    Ok(outcome)
}

pub(super) fn produce(
    captured: &CapturedReleaseSource,
    image: &str,
    architecture: LinuxTarget,
    destination: &Path,
) -> Result<InputFuzzOutcome, FuzzError> {
    let image = ImageIdentity::parse(image).ok_or(FuzzError::Plan)?;
    let publication = publication(destination)?;
    println!(
        "input fuzz reusable artifacts: {}",
        publication.staging().display()
    );
    io::stdout().flush()?;
    let outcome = execute(
        captured,
        publication.staging(),
        image.as_str(),
        architecture,
    )?;
    publication
        .publish_completed(Ok(outcome))
        .map(|(_, outcome)| outcome)
        .map_err(FuzzError::Io)
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
    architecture: LinuxTarget,
) -> Result<InputFuzzOutcome, FuzzError> {
    let image = ImageIdentity::parse(image).ok_or(FuzzError::Plan)?;
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    profile(&metadata, image.as_str(), architecture)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if current != metadata.files {
        return Err(invalid("input fuzz retained bytes or inventory differ"));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let artifact = PublishedNativeArtifact::read(directory, &expected)?;
    check_inputs(
        captured,
        artifact.directory(),
        image.as_str(),
        architecture,
        &metadata.execution,
    )
}
