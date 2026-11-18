use super::{
    combine, CapturedReleaseSource, Failure, ReferenceOutcome, ReferenceRunError, Workspace,
};
use crate::{
    process::CommandSupervisor,
    reference::{ReferenceImage, ReferenceNativeContext, ReferenceRunner},
    release_check::{
        docker_tools,
        native_artifacts::{
            fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
            PublishedNativeArtifact,
        },
        server,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const METADATA: &str = "native-check.toml";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    image: String,
    os: String,
    architecture: String,
    original_source: PathBuf,
    context: ReferenceNativeContext,
    files: Vec<FileFingerprint>,
}

fn failure(stage: &'static str, error: io::Error) -> ReferenceRunError {
    ReferenceRunError::new(stage, Failure::Io(error))
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn version(captured: &CapturedReleaseSource) -> io::Result<String> {
    let bytes = captured
        .files()
        .find(|(path, _)| *path == Path::new("Cargo.toml"))
        .map(|(_, bytes)| bytes)
        .ok_or_else(|| invalid("reference release manifest is missing"))?;
    let manifest: toml::Value =
        toml::from_str(std::str::from_utf8(bytes).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    let version = manifest
        .get("workspace")
        .and_then(|table| table.get("package"))
        .and_then(|table| table.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid("reference release version is missing"))?;
    semver::Version::parse(version).map_err(io::Error::other)?;
    Ok(version.to_owned())
}

fn profile(metadata: &Metadata, image: &ReferenceImage) -> io::Result<()> {
    let source = &metadata.original_source;
    let text = source
        .to_str()
        .ok_or_else(|| invalid("original reference source is not UTF-8"))?;
    if metadata.format != 1
        || metadata.kind != "reference"
        || metadata.image != image.as_str()
        || !source.is_absolute()
        || text.len() > 4096
        || text
            .chars()
            .any(|value| value.is_control() || matches!(value, '\\' | ':'))
        || text.split('/').any(|part| matches!(part, "." | ".."))
        || source
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || source.file_name().and_then(|name| name.to_str()) != Some("source")
        || source
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.starts_with("rusterisk-reference-artifact-"))
    {
        return Err(invalid(
            "reference artifact format, image, or original paths differ",
        ));
    }
    Ok(())
}

fn check(
    captured: &CapturedReleaseSource,
    image: &ReferenceImage,
    root: &Path,
    metadata: &Metadata,
) -> io::Result<()> {
    profile(metadata, image)?;
    server::check_tools(
        captured,
        &root.join("tools"),
        &metadata.os,
        &metadata.architecture,
    )?;
    docker_tools::check(root, &metadata.os, &metadata.architecture)?;
    let source = root.join("native/release-source");
    captured.check(&source).map_err(io::Error::other)?;
    metadata.context.validate(
        &root.join("native/state"),
        &metadata.original_source,
        image,
        &version(captured)?,
        &source,
    )
}

fn tools(captured: &CapturedReleaseSource, root: &Path) -> Result<(), ReferenceRunError> {
    let mut supervisor = CommandSupervisor::new().map_err(|error| {
        ReferenceRunError::new("reference tool supervisor", Failure::Process(error))
    })?;
    let operation = (|| {
        server::capture_tools(captured, root, &mut supervisor)?;
        docker_tools::capture(root, &mut supervisor)
    })()
    .map_err(|error| failure("reference native tool capture", error));
    combine(
        operation,
        supervisor.finish().map_err(|error| {
            ReferenceRunError::new("reference tool supervisor cleanup", Failure::Process(error))
        }),
    )
}

fn native(
    captured: &CapturedReleaseSource,
    image: &ReferenceImage,
    workspace: &Workspace,
    root: &Path,
) -> Result<ReferenceNativeContext, ReferenceRunError> {
    tools(captured, root)?;
    let mut runner = ReferenceRunner::new(workspace.state.clone())
        .map_err(|error| ReferenceRunError::new("reference supervisor", Failure::Process(error)))?;
    let operation = (|| {
        let inspected = runner
            .inspect_image(&workspace.source, image)
            .map_err(|error| {
                ReferenceRunError::new("reference image inspection", Failure::Process(error))
            })?;
        let selected = ReferenceImage::parse_inspection(&inspected).map_err(|_| {
            ReferenceRunError::new(
                "reference image inspection",
                Failure::Contract("reference inspection is not an immutable image"),
            )
        })?;
        if selected.as_str() != image.as_str() {
            return Err(ReferenceRunError::new(
                "reference image inspection",
                Failure::Contract("reference image differs from the selected image"),
            ));
        }
        runner
            .compare_retained(&workspace.source, image)
            .map_err(|error| {
                ReferenceRunError::new("reference comparisons", Failure::Process(error))
            })
    })();
    combine(
        operation,
        runner.finish().map_err(|error| {
            ReferenceRunError::new("reference supervisor cleanup", Failure::Process(error))
        }),
    )
}

fn execute(
    captured: &CapturedReleaseSource,
    image: &ReferenceImage,
    workspace: &Workspace,
    root: &Path,
) -> Result<ReferenceNativeContext, ReferenceRunError> {
    let operation = (|| {
        captured
            .write(&workspace.source, &workspace.state)
            .map_err(|error| {
                ReferenceRunError::new("reference source clone", Failure::Source(error.to_string()))
            })?;
        native(captured, image, workspace, root)
    })();
    let operation = combine(
        operation,
        captured.check(&workspace.source).map_err(|error| {
            ReferenceRunError::new(
                "reference final source check",
                Failure::Source(error.to_string()),
            )
        }),
    );
    let operation = operation.and_then(|context| {
        let native = root.join("native");
        fs::create_dir(&native).map_err(|error| failure("reference native directory", error))?;
        context
            .preserve(&workspace.state, &native.join("state"))
            .map_err(|error| failure("reference native input preservation", error))?;
        captured
            .write(
                &native.join("release-source"),
                &native.join("release-state"),
            )
            .map_err(|error| {
                ReferenceRunError::new(
                    "reference source preservation",
                    Failure::Source(error.to_string()),
                )
            })?;
        Ok(context)
    });
    combine(
        operation,
        fs::remove_dir_all(&workspace.root)
            .map_err(|error| failure("reference owned state cleanup", error)),
    )
}

pub(super) fn produce<'a>(
    captured: &'a CapturedReleaseSource,
    image: &'a ReferenceImage,
    published: &Path,
) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| failure("reference execution clock", io::Error::other(error)))?
        .as_nanos();
    let name = format!(
        "rusterisk-reference-artifact-{}-{stamp}",
        std::process::id()
    );
    let parent = published.parent().ok_or_else(|| {
        failure(
            "reference artifact destination",
            invalid("reference destination requires an absolute path"),
        )
    })?;
    let staging = parent.join(format!("{name}.staging"));
    let publication = NativeArtifactPublication::create(&staging, published)
        .map_err(|error| failure("reference artifact destination", error))?;
    let directory = fs::canonicalize(std::env::temp_dir())
        .map_err(|error| failure("reference temporary root", error))?
        .join(name);
    let workspace = Workspace::create(&directory)?;
    let original_source = workspace.source.clone();
    let completed =
        execute(captured, image, &workspace, publication.staging()).and_then(|context| {
            let metadata = Metadata {
                format: 1,
                kind: "reference".to_owned(),
                image: image.as_str().to_owned(),
                os: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
                original_source,
                context,
                files: fingerprints(publication.staging())
                    .map_err(|error| failure("reference native inventory", error))?,
            };
            check(captured, image, publication.staging(), &metadata)
                .map_err(|error| failure("reference native artifact validation", error))?;
            let text = toml::to_string(&metadata)
                .map_err(|error| failure("reference artifact metadata", io::Error::other(error)))?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(publication.staging().join(METADATA))
                .map_err(|error| failure("reference artifact metadata", error))?;
            file.write_all(text.as_bytes())
                .and_then(|()| file.flush())
                .map_err(|error| failure("reference artifact metadata", error))?;
            Ok(ReferenceOutcome { captured, image })
        });
    publication
        .publish_completed(completed.map_err(io::Error::other))
        .map(|(_, outcome)| outcome)
        .map_err(|error| failure("reference artifact publication", error))
}

pub(super) fn read<'a>(
    captured: &'a CapturedReleaseSource,
    image: &'a ReferenceImage,
    root: &Path,
) -> Result<ReferenceOutcome<'a>, ReferenceRunError> {
    let operation = (|| {
        let current = fingerprints(root)?;
        let bytes = read_regular(&root.join(METADATA), 8 * 1024 * 1024)?;
        let metadata: Metadata =
            toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
        if current
            .into_iter()
            .filter(|file| file.path() != METADATA)
            .collect::<Vec<_>>()
            != metadata.files
        {
            return Err(invalid(
                "reference artifact retained bytes or inventory differ",
            ));
        }
        let expected: Vec<_> = metadata
            .files
            .iter()
            .map(FileFingerprint::path)
            .chain(std::iter::once(METADATA))
            .collect();
        let artifact = PublishedNativeArtifact::read(root, &expected)?;
        check(captured, image, artifact.directory(), &metadata)
    })();
    operation.map_err(|error| failure("reference artifact reader", error))?;
    Ok(ReferenceOutcome { captured, image })
}
