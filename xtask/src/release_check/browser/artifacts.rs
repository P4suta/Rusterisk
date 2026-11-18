use super::{BrowserOutcome, CapturedReleaseSource, RunError};
use crate::{
    browser::{self, BrowserExecution, PreparedBrowserRun, SourceSnapshot},
    process::CommandSupervisor,
    release_check::{
        docker_tools,
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
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const METADATA: &str = "native-check.toml";

#[cfg(test)]
#[path = "artifacts/tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    image: String,
    reference: Option<String>,
    os: String,
    architecture: String,
    execution: BrowserExecution,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn error(stage: &'static str, failure: io::Error) -> RunError {
    RunError::Retained {
        preceding: None,
        stage,
        failure,
    }
}

fn follow<T>(
    operation: Result<T, RunError>,
    follow_up: io::Result<()>,
    stage: &'static str,
) -> Result<T, RunError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(failure)) => Err(error(stage, failure)),
        (Err(operation), Err(failure)) => Err(RunError::Retained {
            preceding: Some(operation.to_string()),
            stage,
            failure,
        }),
    }
}

fn bound(captured: &CapturedReleaseSource, source: &Path) -> Result<SourceSnapshot, RunError> {
    captured.check(source).map_err(|failure| {
        error(
            "browser captured release input check",
            io::Error::other(failure),
        )
    })?;
    let snapshot = SourceSnapshot::read(source)?;
    let inputs = SourceInputs::new(snapshot.files())
        .map_err(|failure| error("browser input projection", io::Error::other(failure)))?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map_err(|failure| error("browser release input binding", io::Error::other(failure)))?;
    drop(inputs);
    Ok(snapshot)
}

fn bind_prepared(
    captured: &CapturedReleaseSource,
    prepared: &PreparedBrowserRun,
) -> Result<(), RunError> {
    let inputs = SourceInputs::new(prepared.inputs()).map_err(|failure| {
        error(
            "browser prepared input projection",
            io::Error::other(failure),
        )
    })?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map(|_| ())
        .map_err(|failure| {
            error(
                "browser prepared release input binding",
                io::Error::other(failure),
            )
        })
}

fn tools(root: &Path) -> Result<(), RunError> {
    fs::create_dir(root.join("tools"))?;
    let mut supervisor = CommandSupervisor::new().map_err(RunError::Process)?;
    let operation = docker_tools::capture(root, &mut supervisor)
        .map_err(|failure| error("browser native Docker identity", failure));
    follow(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "browser tool supervisor cleanup",
    )
}

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn create() -> io::Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for attempt in 0..32 {
            let root = parent.join(format!(
                "rusterisk-browser-artifact-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => {}
                Err(failure) => return Err(failure),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "browser artifact workspace reservation failed",
        ))
    }

    fn source(&self) -> PathBuf {
        self.root.join("source")
    }

    fn close(self) -> io::Result<()> {
        if !fs::symlink_metadata(&self.root)?.is_dir() || fs::canonicalize(&self.root)? != self.root
        {
            return Err(invalid("browser artifact workspace identity changed"));
        }
        fs::remove_dir_all(self.root)
    }
}

fn publication(destination: &Path) -> io::Result<NativeArtifactPublication> {
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("browser artifact destination has no parent"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let staging = parent.join(format!(
            "browser-artifact-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match NativeArtifactPublication::create(&staging, destination) {
            Ok(publication) => return Ok(publication),
            Err(failure)
                if failure.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&staging).is_ok()
                    && fs::symlink_metadata(destination)
                        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) => {}
            Err(failure) => return Err(failure),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "browser artifact staging reservation failed",
    ))
}

fn profile(metadata: &Metadata, image: &str, reference: Option<&str>) -> Result<(), RunError> {
    let kind = if reference.is_some() {
        "browser-comparison"
    } else {
        "browser"
    };
    if metadata.format != 1
        || metadata.kind != kind
        || metadata.image != image
        || metadata.reference.as_deref() != reference
    {
        return Err(error(
            "browser artifact profile",
            invalid("browser artifact format, gate, or immutable image differs"),
        ));
    }
    Ok(())
}

fn check(
    captured: &CapturedReleaseSource,
    root: &Path,
    metadata: &Metadata,
    image: &str,
    reference: Option<&str>,
) -> Result<BrowserOutcome, RunError> {
    profile(metadata, image, reference)?;
    docker_tools::check(root, &metadata.os, &metadata.architecture)
        .map_err(|failure| error("browser retained Docker identity", failure))?;
    let snapshot = bound(captured, &root.join("native/release-source"))?;
    match reference {
        Some(reference) => metadata.execution.read_selected(
            snapshot,
            image,
            Some(reference),
            &root.join("native/browser"),
        ),
        None => browser::read_retained(
            snapshot,
            image,
            &root.join("native/browser"),
            &metadata.execution,
        ),
    }
}

fn execute(
    captured: &CapturedReleaseSource,
    image: &str,
    reference: Option<&str>,
    root: &Path,
) -> Result<BrowserExecution, RunError> {
    let workspace = Workspace::create()?;
    let operation = (|| {
        tools(root)?;
        captured
            .write(
                &workspace.source(),
                &workspace.root.join("captured-browser"),
            )
            .map_err(|failure| {
                error(
                    "browser release source materialization",
                    io::Error::other(failure),
                )
            })?;
        let prepared = PreparedBrowserRun::read(&workspace.source(), image)?;
        let prepared = match reference {
            Some(reference) => prepared.with_reference(reference)?,
            None => prepared,
        };
        bind_prepared(captured, &prepared)?;
        fs::create_dir(root.join("native"))?;
        let retained = browser::run_retained(prepared, &root.join("native/browser"))?;
        captured
            .write(
                &root.join("native/release-source"),
                &root.join("native/captured-browser"),
            )
            .map_err(|failure| {
                error(
                    "browser release source preservation",
                    io::Error::other(failure),
                )
            })?;
        Ok(retained.into_execution())
    })();
    let checked = captured
        .check(&workspace.source())
        .map_err(io::Error::other);
    let operation = follow(operation, checked, "browser final release input check");
    follow(
        operation,
        workspace.close(),
        "browser owned release workspace cleanup",
    )
}

pub(super) fn produce(
    captured: &CapturedReleaseSource,
    image: &str,
    reference: Option<&str>,
    destination: &Path,
) -> Result<BrowserOutcome, RunError> {
    let publication = publication(destination)?;
    let completed =
        execute(captured, image, reference, publication.staging()).and_then(|execution| {
            let metadata = Metadata {
                format: 1,
                kind: if reference.is_some() {
                    "browser-comparison"
                } else {
                    "browser"
                }
                .to_owned(),
                image: image.to_owned(),
                reference: reference.map(str::to_owned),
                os: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
                execution,
                files: fingerprints(publication.staging())?,
            };
            check(captured, publication.staging(), &metadata, image, reference)?;
            let bytes = toml::to_string(&metadata).map_err(io::Error::other)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(publication.staging().join(METADATA))?;
            file.write_all(bytes.as_bytes())?;
            file.flush()?;
            Ok(())
        });
    let (published, ()) = publication.publish_completed(completed.map_err(io::Error::other))?;
    read(captured, published.directory(), image, reference)
}

pub(super) fn read(
    captured: &CapturedReleaseSource,
    directory: &Path,
    image: &str,
    reference: Option<&str>,
) -> Result<BrowserOutcome, RunError> {
    let current = fingerprints(directory)?;
    let bytes = read_regular(&directory.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if current != metadata.files {
        return Err(error(
            "browser retained inventory",
            invalid("browser retained files or bytes differ"),
        ));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let published = PublishedNativeArtifact::read(directory, &expected)?;
    check(captured, published.directory(), &metadata, image, reference)
}
