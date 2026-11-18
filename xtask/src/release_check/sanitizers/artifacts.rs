use super::{read_bound_native, CapturedReleaseSource, NativeError, NativeOutcome};
use crate::{
    native_sanitizers::{plan, NativeExecution},
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
    execution: NativeExecution,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> NativeError {
    io::Error::new(io::ErrorKind::InvalidData, reason).into()
}

fn profile(metadata: &Metadata, image: &str) -> Result<(), NativeError> {
    if metadata.format != FORMAT || metadata.kind != "native-sanitizers" || metadata.image != image
    {
        return Err(invalid(
            "native sanitizer artifact format, gate, or image differs",
        ));
    }
    metadata.execution.check(image)?;
    Ok(())
}

fn publication(destination: &Path) -> Result<NativeArtifactPublication, NativeError> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("native sanitizer artifact destination has no parent"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let staging = parent.join(format!(
            "sanitizer-artifact-{}-{stamp}-{attempt}",
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
        "native sanitizer staging reservation limit exceeded",
    )
    .into())
}

fn check_inputs(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
    execution: &NativeExecution,
) -> Result<NativeOutcome, NativeError> {
    read_bound_native(captured, &directory.join("native/release-source"))?.read_retained(
        image,
        &directory.join("native/sanitizers"),
        execution,
    )
}

fn record(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
    execution: NativeExecution,
) -> Result<(), NativeError> {
    check_inputs(captured, directory, image, &execution)?;
    let metadata = Metadata {
        format: FORMAT,
        kind: "native-sanitizers".to_owned(),
        image: image.to_owned(),
        execution,
        files: fingerprints(directory)?,
    };
    profile(&metadata, image)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(METADATA))?;
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
    directory: &Path,
    image: &str,
) -> Result<(), NativeError> {
    fs::create_dir(directory.join("native"))?;
    let source = directory.join("native/release-source");
    captured
        .write(&source, &directory.join("native/captured-browser"))
        .map_err(io::Error::other)?;
    let capture = read_bound_native(captured, &source)?;
    let operation = capture.verify_retained(image, &directory.join("native/sanitizers"));
    let final_check = captured
        .check(&source)
        .map_err(io::Error::other)
        .map_err(NativeError::Io);
    let (_, execution) = match (operation, final_check) {
        (operation, Ok(())) => operation?,
        (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(error)) => {
            return Err(NativeError::FollowUp {
                operation: operation.to_string(),
                context: "native sanitizer final release input check failed",
                error: io::Error::other(error),
            })
        }
    };
    record(captured, directory, image, execution)
}

pub(super) fn produce(
    captured: &CapturedReleaseSource,
    image: &str,
    destination: &Path,
) -> Result<NativeOutcome, NativeError> {
    let selected = plan(image)?;
    let image = selected.image();
    let publication = publication(destination)?;
    println!(
        "native sanitizer reusable artifacts: {}",
        publication.staging().display()
    );
    io::stdout().flush()?;
    let completed = execute(captured, publication.staging(), image);
    publication.publish_completed(completed.map_err(io::Error::other))?;
    read(captured, destination, image)
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
) -> Result<NativeOutcome, NativeError> {
    let selected = plan(image)?;
    let image = selected.image();
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    profile(&metadata, image)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if current != metadata.files {
        return Err(invalid("native sanitizer retained files or bytes differ"));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let artifact = PublishedNativeArtifact::read(directory, &expected)?;
    check_inputs(captured, artifact.directory(), image, &metadata.execution)
}
