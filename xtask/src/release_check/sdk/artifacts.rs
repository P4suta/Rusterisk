use super::{
    combine, CapturedReleaseSource, CommandSupervisor, Failure, Paths, SdkOutcome, SdkPackage,
    SdkRunError,
};
use crate::{
    distribution::{check_sdk_consumer_results, verify_sdk_consumers_supervised, SdkConsumerPlan},
    release_check::native_artifacts::{
        fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
        PublishedNativeArtifact,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

mod inputs;
use inputs::Loaded;
mod tools;

const METADATA: &str = "native-check.toml";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    os: String,
    architecture: String,
    consumer_root: PathBuf,
    files: Vec<FileFingerprint>,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn original_plan(root: &Path) -> io::Result<SdkConsumerPlan> {
    let text = root
        .to_str()
        .ok_or_else(|| invalid("original SDK consumer path is not UTF-8"))?;
    if !root.is_absolute()
        || text.len() > 4096
        || text
            .chars()
            .any(|character| character.is_control() || matches!(character, '\\' | ':'))
        || text.split('/').any(|part| matches!(part, "." | ".."))
        || root
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || root
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.starts_with("rusterisk-sdk-consumers-"))
    {
        return Err(invalid(
            "original SDK consumer path differs from the owned workspace profile",
        ));
    }
    SdkConsumerPlan::new(&root.join("work"), &root.join("state"))
}

fn profile(metadata: &Metadata) -> io::Result<()> {
    if metadata.format != FORMAT || metadata.kind != "sdk" {
        return Err(invalid("native SDK artifact format or gate differs"));
    }
    tools::host(&metadata.os, &metadata.architecture)?;
    original_plan(&metadata.consumer_root)?;
    Ok(())
}

fn check_outputs(
    captured: &CapturedReleaseSource,
    root: &Path,
    loaded: &Loaded,
    metadata: &Metadata,
) -> io::Result<()> {
    tools::check(
        captured,
        &root.join("tools"),
        &metadata.os,
        &metadata.architecture,
    )?;
    let plan = original_plan(&metadata.consumer_root)?;
    check_sdk_consumer_results(&loaded.contents, &root.join("native/consumer"), &plan)
        .map_err(io::Error::other)
}

fn record(
    captured: &CapturedReleaseSource,
    loaded: &Loaded,
    root: &Path,
    execution: &Path,
) -> io::Result<()> {
    let metadata = Metadata {
        format: FORMAT,
        kind: "sdk".to_owned(),
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        consumer_root: execution.to_owned(),
        files: fingerprints(root)?,
    };
    profile(&metadata)?;
    captured
        .check(&root.join("native/release-source"))
        .map_err(io::Error::other)?;
    let saved =
        Loaded::read(captured, &root.join("native/sdk-candidate")).map_err(io::Error::other)?;
    loaded.same(&saved)?;
    check_outputs(captured, root, &saved, &metadata)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(METADATA))?;
    file.write_all(
        toml::to_string(&metadata)
            .map_err(io::Error::other)?
            .as_bytes(),
    )?;
    file.flush()
}

fn execute(
    captured: &CapturedReleaseSource,
    loaded: &Loaded,
    root: &Path,
    execution: &Path,
) -> Result<(), SdkRunError> {
    let native = root.join("native");
    fs::create_dir(&native)
        .map_err(|error| SdkRunError::new("SDK native artifact directory", Failure::Io(error)))?;
    let development = tools::channel(captured)
        .map_err(|error| SdkRunError::new("SDK captured toolchain", Failure::Io(error)))?;
    let mut supervisor = CommandSupervisor::new()
        .map_err(|error| SdkRunError::new("SDK artifact supervisor", Failure::Process(error)))?;
    let operation = match tools::capture(captured, root, &mut supervisor) {
        Ok(()) => verify_sdk_consumers_supervised(
            &loaded.contents,
            execution,
            &native.join("consumer"),
            tools::Port {
                supervisor,
                development: &development,
            },
        )
        .map_err(|error| SdkRunError::new("SDK native consumers", Failure::Consumer(error))),
        Err(error) => combine(
            Err(SdkRunError::new("SDK native tools", Failure::Io(error))),
            supervisor.finish().map_err(|error| {
                SdkRunError::new("SDK supervisor cleanup", Failure::Process(error))
            }),
        ),
    };
    let operation = combine(
        operation,
        loaded.check(captured).map_err(|error| {
            SdkRunError::new("SDK source after native execution", Failure::Io(error))
        }),
    );
    let operation = combine(
        operation,
        loaded
            .preserve(captured, &native)
            .map_err(|error| SdkRunError::new("SDK native input preservation", Failure::Io(error))),
    );
    operation?;
    record(captured, loaded, root, execution)
        .map_err(|error| SdkRunError::new("SDK native result validation", Failure::Io(error)))
}

pub(super) fn produce<'a>(
    captured: &'a CapturedReleaseSource,
    candidate: &Path,
    published: &Path,
) -> Result<SdkOutcome<'a>, SdkRunError> {
    let paths = Paths::new(candidate)?;
    let loaded = Loaded::read(captured, &paths.candidate)?;
    let parent = published.parent().ok_or_else(|| {
        SdkRunError::new(
            "SDK artifact destination",
            Failure::Io(invalid("SDK artifact destination has no parent")),
        )
    })?;
    let identity = paths.artifacts.file_name().ok_or_else(|| {
        SdkRunError::new(
            "SDK artifact staging",
            Failure::Io(invalid("SDK staging identity is missing")),
        )
    })?;
    let staging = parent.join(identity);
    let input = fs::canonicalize(candidate)
        .map_err(|error| SdkRunError::new("SDK artifact input", Failure::Io(error)))?;
    if [staging.as_path(), published]
        .iter()
        .any(|path| path.starts_with(&input) || input.starts_with(path))
    {
        return Err(SdkRunError::new(
            "SDK artifact destination",
            Failure::Io(invalid(
                "SDK artifact destination overlaps candidate inputs",
            )),
        ));
    }
    let publication = NativeArtifactPublication::create(&staging, published)
        .map_err(|error| SdkRunError::new("SDK artifact destination", Failure::Io(error)))?;
    println!(
        "SDK reusable artifacts: {}",
        publication.staging().display()
    );
    io::stdout()
        .flush()
        .map_err(|error| SdkRunError::new("SDK artifact output", Failure::Io(error)))?;
    let completed =
        execute(captured, &loaded, publication.staging(), &paths.execution).map(|()| SdkOutcome {
            captured,
            projection: loaded.projection,
            version: loaded.contents.version().to_owned(),
            checksums: SdkPackage::ALL.map(|package| loaded.contents.checksum(package).to_owned()),
        });
    publication
        .publish_completed(completed.map_err(io::Error::other))
        .map(|(_, outcome)| outcome)
        .map_err(|error| SdkRunError::new("SDK artifact publication", Failure::Io(error)))
}

pub(super) fn read<'a>(
    captured: &'a CapturedReleaseSource,
    root: &Path,
    candidate: &Path,
) -> io::Result<SdkOutcome<'a>> {
    let current = fingerprints(root)?;
    let bytes = read_regular(&root.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    profile(&metadata)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if metadata.files != current {
        return Err(invalid(
            "SDK native artifact retained file bytes or inventory differ",
        ));
    }
    let expected: Vec<_> = metadata
        .files
        .iter()
        .map(FileFingerprint::path)
        .chain(std::iter::once(METADATA))
        .collect();
    let artifact = PublishedNativeArtifact::read(root, &expected)?;
    let root = artifact.directory();
    captured
        .check(&root.join("native/release-source"))
        .map_err(io::Error::other)?;
    let selected = Loaded::read(captured, candidate).map_err(io::Error::other)?;
    let saved =
        Loaded::read(captured, &root.join("native/sdk-candidate")).map_err(io::Error::other)?;
    selected.same(&saved)?;
    check_outputs(captured, root, &saved, &metadata)?;
    Ok(SdkOutcome {
        captured,
        projection: saved.projection,
        version: saved.contents.version().to_owned(),
        checksums: SdkPackage::ALL.map(|package| saved.contents.checksum(package).to_owned()),
    })
}
