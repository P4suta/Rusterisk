use super::{read_bound_miri, CapturedReleaseSource, CoreMiriError, CoreMiriOutcome};
use crate::{
    core_miri::{check_miri_recipe, MiriExecution},
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
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    image: String,
    execution: MiriExecution,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> CoreMiriError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

fn recipe(captured: &CapturedReleaseSource) -> Result<(), CoreMiriError> {
    let bytes = captured
        .files()
        .find(|(path, _)| *path == Path::new("containers/input-verification/Dockerfile"))
        .map(|(_, bytes)| bytes)
        .ok_or_else(|| invalid("captured Miri image recipe is missing"))?;
    check_miri_recipe(bytes)
}

fn profile(metadata: &Metadata, image: &ImageIdentity) -> Result<(), CoreMiriError> {
    if metadata.format != FORMAT || metadata.kind != "core-miri" || metadata.image != image.as_str()
    {
        return Err(invalid(
            "native Miri artifact format, gate, or immutable image differs",
        ));
    }
    metadata.execution.check(image.as_str())
}

fn publication(destination: &Path) -> Result<NativeArtifactPublication, CoreMiriError> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("Miri artifact destination has no parent"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let staging = parent.join(format!(
            "miri-artifact-{}-{stamp}-{attempt}",
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
        "Miri artifact staging reservation limit exceeded",
    )
    .into())
}

fn check_inputs(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &str,
    execution: &MiriExecution,
) -> Result<CoreMiriOutcome, CoreMiriError> {
    let capture = read_bound_miri(captured, &root.join("native/release-source"))?;
    recipe(captured)?;
    capture.read_retained(image, &root.join("native/miri"), execution)
}

fn record(
    captured: &CapturedReleaseSource,
    root: &Path,
    image: &ImageIdentity,
    execution: MiriExecution,
) -> Result<(), CoreMiriError> {
    check_inputs(captured, root, image.as_str(), &execution)?;
    let metadata = Metadata {
        format: FORMAT,
        kind: "core-miri".to_owned(),
        image: image.as_str().to_owned(),
        execution,
        files: fingerprints(root)?,
    };
    profile(&metadata, image)?;
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
    image: &ImageIdentity,
) -> Result<CoreMiriOutcome, CoreMiriError> {
    fs::create_dir(root.join("native"))?;
    let source = root.join("native/release-source");
    captured
        .write(&source, &root.join("native/captured-browser"))
        .map_err(io::Error::other)?;
    let capture = read_bound_miri(captured, &source)?;
    let operation = capture.verify_retained(image.as_str(), &root.join("native/miri"));
    let final_check = captured
        .check(&source)
        .map_err(io::Error::other)
        .map_err(CoreMiriError::Io);
    let (outcome, execution) = match (operation, final_check) {
        (operation, Ok(())) => operation?,
        (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(follow_up)) => {
            return Err(CoreMiriError::FollowUp {
                operation: operation.to_string(),
                context: "Miri final release input check failed",
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
) -> Result<CoreMiriOutcome, CoreMiriError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMiriError::Plan)?;
    recipe(captured)?;
    let publication = publication(destination)?;
    println!(
        "Miri reusable artifacts: {}",
        publication.staging().display()
    );
    io::stdout().flush()?;
    let outcome = execute(captured, publication.staging(), &image)?;
    publication
        .publish_completed(Ok(outcome))
        .map(|(_, outcome)| outcome)
        .map_err(CoreMiriError::Io)
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
) -> Result<CoreMiriOutcome, CoreMiriError> {
    let image = ImageIdentity::parse(image).ok_or(CoreMiriError::Plan)?;
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    profile(&metadata, &image)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if current != metadata.files {
        return Err(invalid(
            "Miri retained native artifact files or bytes differ",
        ));
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
        &metadata.execution,
    )
}
