use super::{CapturedReleaseSource, CoreMutationError, CoreMutationOutcome};
use crate::{
    core_mutation::{CapturedCoreMutation, MutationExecution},
    reference::ImageIdentity,
    release_check::{
        native_artifacts::{
            fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
            PublishedNativeArtifact,
        },
        SourceInputs,
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
    execution: MutationExecution,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> CoreMutationError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

fn capture(
    captured: &CapturedReleaseSource,
    source: &Path,
) -> Result<CapturedCoreMutation, CoreMutationError> {
    captured.check(source).map_err(io::Error::other)?;
    let mutation = CapturedCoreMutation::read(source)?;
    let inputs = SourceInputs::new(mutation.inputs()).map_err(io::Error::other)?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map_err(io::Error::other)?;
    Ok(mutation)
}

fn check(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &str,
    execution: &MutationExecution,
) -> Result<CoreMutationOutcome, CoreMutationError> {
    capture(captured, &root.join("native/release-source"))?.read_retained(
        image,
        &root.join("native/mutation"),
        execution,
    )
}

fn record(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &ImageIdentity,
    execution: MutationExecution,
) -> Result<(), CoreMutationError> {
    check(captured, root, image.as_str(), &execution)?;
    let metadata = Metadata {
        format: 1,
        kind: "core-mutation".to_owned(),
        image: image.as_str().to_owned(),
        execution,
        files: fingerprints(root)?,
    };
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

fn publication(destination: &Path) -> Result<NativeArtifactPublication, CoreMutationError> {
    let parent = destination.parent().ok_or(CoreMutationError::Plan)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let staging = parent.join(format!(
            "mutation-artifact-{}-{stamp}-{attempt}",
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
        "mutation artifact staging reservation limit exceeded",
    )
    .into())
}

fn execute(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &ImageIdentity,
) -> Result<CoreMutationOutcome, CoreMutationError> {
    fs::create_dir(root.join("native"))?;
    let source = root.join("native/release-source");
    captured
        .write(&source, &root.join("native/captured-browser"))
        .map_err(io::Error::other)?;
    let operation =
        capture(captured, &source)?.verify_retained(image.as_str(), &root.join("native/mutation"));
    let final_check = captured
        .check(&source)
        .map_err(io::Error::other)
        .map_err(CoreMutationError::Io);
    let (outcome, execution) = match (operation, final_check) {
        (operation, Ok(())) => operation?,
        (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(follow_up)) => {
            return Err(CoreMutationError::FollowUp {
                operation: operation.to_string(),
                context: "mutation final release input check failed",
                follow_up: follow_up.to_string(),
            })
        }
    };
    record(captured, root, image, execution)?;
    Ok(outcome)
}

pub(super) fn produce(
    captured: &CapturedReleaseSource,
    image: &str,
    destination: &Path,
) -> Result<CoreMutationOutcome, CoreMutationError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMutationError::Plan)?;
    let publication = publication(destination)?;
    println!(
        "Mutation reusable artifacts: {}",
        publication.staging().display()
    );
    io::stdout().flush()?;
    let outcome = execute(captured, publication.staging(), &image)?;
    publication
        .publish_completed(Ok(outcome))
        .map(|(_, outcome)| outcome)
        .map_err(CoreMutationError::Io)
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
) -> Result<CoreMutationOutcome, CoreMutationError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMutationError::Plan)?;
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    if metadata.format != 1 || metadata.kind != "core-mutation" || metadata.image != image.as_str()
    {
        return Err(invalid(
            "mutation retained artifact format, gate, or image differs",
        ));
    }
    metadata.execution.check(image.as_str())?;
    if current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect::<Vec<_>>()
        != metadata.files
    {
        return Err(invalid("mutation retained native files or bytes differ"));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let artifact = PublishedNativeArtifact::read(directory, &expected)?;
    check(
        captured,
        artifact.directory(),
        image.as_str(),
        &metadata.execution,
    )
}
