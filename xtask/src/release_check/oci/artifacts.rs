use super::{
    graph_matches, verify_oci_recorded, CapturedReleaseSource, CommandSupervisor, LinuxOciTarget,
    Loaded, OciOutcome, OciRequest, OciRunError, Paths, Retention, RunError, SdkPackage,
};
use crate::{
    distribution::{check_sdk_http_results, SdkConsumerPlan},
    release_check::{
        candidate_run::checksum,
        native_artifacts::{
            fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
            PublishedNativeArtifact,
        },
        server,
    },
    server_distribution::{
        check_oci_runtime_outputs, OciEngineProfile, OciNativeContext, OciRuntimeInputs,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
};

use crate::release_check::docker_tools;

const METADATA: &str = "native-check.toml";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    target: String,
    os: String,
    architecture: String,
    identity: String,
    engine: OciEngineProfile,
    execution_root: PathBuf,
    consumer_root: PathBuf,
    http: SocketAddr,
    sip: SocketAddr,
    files: Vec<FileFingerprint>,
}

struct OriginalPaths {
    configuration: PathBuf,
    consumer: SdkConsumerPlan,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn original_path(path: &Path) -> io::Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("original OCI path is not UTF-8"))?;
    if !path.is_absolute()
        || text.len() > 4096
        || text
            .chars()
            .any(|value| value.is_control() || matches!(value, '\\' | ':'))
        || text.split('/').any(|part| matches!(part, "." | ".."))
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid(
            "original OCI paths require bounded absolute components",
        ));
    }
    Ok(())
}

impl OriginalPaths {
    fn new(metadata: &Metadata) -> io::Result<Self> {
        let execution = &metadata.execution_root;
        let consumer = &metadata.consumer_root;
        original_path(execution)?;
        original_path(consumer)?;
        if execution.starts_with(consumer)
            || consumer.starts_with(execution)
            || execution
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !name.starts_with("rusterisk-oci-artifact-"))
            || consumer.file_name().and_then(|name| name.to_str()) != Some("consumer")
        {
            return Err(invalid(
                "original OCI paths differ from the owned workspace profile",
            ));
        }
        Ok(Self {
            configuration: execution.join("config"),
            consumer: SdkConsumerPlan::new(&consumer.join("work"), &consumer.join("state"))?,
        })
    }
}

pub(super) fn capture_tools(
    captured: &CapturedReleaseSource,
    root: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<()> {
    server::capture_tools(captured, root, supervisor)?;
    docker_tools::capture(root, supervisor)
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub(super) fn preserve_context(
    request: &OciRequest<'_>,
    context: &OciNativeContext,
) -> io::Result<()> {
    let source = request.execution.join("config");
    let destination = request.artifacts.join("runtime-config");
    fs::create_dir(&destination)?;
    let mut count = 0usize;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        count += 1;
        if count > 5 || !entry.file_type()?.is_file() {
            return Err(invalid("OCI runtime configuration inventory differs"));
        }
        write(
            &destination.join(entry.file_name()),
            &read_regular(&entry.path(), 65_536)?,
        )?;
    }
    if count != 5 || context.http().port() == 0 || context.sip().port() == 0 {
        return Err(invalid(
            "OCI runtime configuration or listeners are incomplete",
        ));
    }
    Ok(())
}

fn same_candidates(selected: &Loaded, saved: &Loaded) -> io::Result<()> {
    if selected.server_projection != saved.server_projection
        || selected.sdk_projection != saved.sdk_projection
        || selected.server.checksum() != saved.server.checksum()
        || selected.server.source_digest() != saved.server.source_digest()
        || selected.server.name().version() != saved.server.name().version()
        || selected.sdk.version() != saved.sdk.version()
        || selected.sdk.source_digest() != saved.sdk.source_digest()
        || SdkPackage::ALL
            .iter()
            .any(|package| selected.sdk.checksum(*package) != saved.sdk.checksum(*package))
    {
        return Err(invalid(
            "OCI artifact candidate bytes or source projections differ",
        ));
    }
    Ok(())
}

fn check_outputs(
    captured: &CapturedReleaseSource,
    root: &Path,
    loaded: &Loaded,
    metadata: &Metadata,
) -> io::Result<()> {
    if metadata.format != FORMAT
        || metadata.kind != "oci"
        || metadata.target != loaded.server.name().target().triple()
    {
        return Err(invalid(
            "native OCI artifact format, gate, or target differs",
        ));
    }
    server::check_tools(
        captured,
        &root.join("tools"),
        &metadata.os,
        &metadata.architecture,
    )?;
    docker_tools::check(root, &metadata.os, &metadata.architecture)?;
    let paths = OriginalPaths::new(metadata)?;
    let native = root.join("native/runtime");
    let checked = loaded.server.check_oci(&native.join("image.oci.tar"))?;
    check_oci_runtime_outputs(
        &loaded.server,
        &checked,
        &native,
        &OciRuntimeInputs {
            identity: &metadata.identity,
            http: metadata.http,
            sip: metadata.sip,
            profile: metadata.engine,
            configuration: &paths.configuration,
        },
    )?;
    check_sdk_http_results(&loaded.sdk, &native.join("consumer"), &paths.consumer)
        .map_err(io::Error::other)
}

pub(super) fn record(
    captured: &CapturedReleaseSource,
    loaded: &Loaded,
    request: &OciRequest<'_>,
    context: &OciNativeContext,
    root: &Path,
) -> io::Result<()> {
    let metadata = Metadata {
        format: FORMAT,
        kind: "oci".to_owned(),
        target: request.target.server_target().triple().to_owned(),
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        identity: context.identity().to_owned(),
        engine: context.profile(),
        execution_root: request.execution.to_owned(),
        consumer_root: request.consumer_execution.join("consumer"),
        http: context.http(),
        sip: context.sip(),
        files: fingerprints(root)?,
    };
    let native = root.join("native/runtime");
    captured
        .check(&native.join("release-source"))
        .map_err(io::Error::other)?;
    let saved = Loaded::read(
        captured,
        &native.join("server-candidate"),
        request.target.server_target(),
        &native.join("sdk-candidate"),
    )
    .map_err(io::Error::other)?;
    same_candidates(loaded, &saved)?;
    check_outputs(captured, root, &saved, &metadata)?;
    write(
        &root.join(METADATA),
        toml::to_string(&metadata)
            .map_err(io::Error::other)?
            .as_bytes(),
    )
}

pub(super) fn produce<'a>(
    captured: &'a CapturedReleaseSource,
    candidate: &Path,
    target: LinuxOciTarget,
    archive: &Path,
    sdk_candidate: &Path,
    published: &Path,
) -> Result<OciOutcome<'a>, OciRunError> {
    let loaded = Loaded::read(captured, candidate, target.server_target(), sdk_candidate)
        .map_err(OciRunError)?;
    loaded
        .server
        .check_oci(archive)
        .map_err(|error| OciRunError(RunError::io("OCI candidate graph", error)))?;
    let paths = Paths::new(candidate, "oci-artifact").map_err(OciRunError)?;
    let parent = published.parent().ok_or_else(|| {
        OciRunError(RunError::contract(
            "OCI artifact destination",
            "publication destination has no parent",
        ))
    })?;
    let identity = paths.artifacts.file_name().ok_or_else(|| {
        OciRunError(RunError::contract(
            "OCI artifact staging",
            "staging identity is missing",
        ))
    })?;
    let staging = parent.join(identity);
    for input in [candidate, sdk_candidate, archive] {
        let input = fs::canonicalize(input)
            .map_err(|error| OciRunError(RunError::io("OCI artifact input", error)))?;
        if [staging.as_path(), published]
            .iter()
            .any(|path| path.starts_with(&input) || input.starts_with(path))
        {
            return Err(OciRunError(RunError::contract(
                "OCI artifact destination",
                "publication destination overlaps candidate inputs",
            )));
        }
    }
    let publication = NativeArtifactPublication::create(&staging, published)
        .map_err(|error| OciRunError(RunError::io("OCI artifact destination", error)))?;
    fs::create_dir(publication.staging().join("native"))
        .map_err(|error| OciRunError(RunError::io("OCI native artifact directory", error)))?;
    let supervisor = CommandSupervisor::new()
        .map_err(|error| OciRunError(RunError::process("OCI artifact supervisor", error)))?;
    let completed = verify_oci_recorded(
        captured,
        OciRequest {
            candidate,
            target,
            archive,
            sdk_candidate,
            execution: &paths.execution,
            consumer_execution: &paths.consumer_execution,
            artifacts: &publication.staging().join("native/runtime"),
        },
        supervisor,
        |_, _| Ok(()),
        Retention::Reusable(publication.staging()),
    );
    publication
        .publish_completed(completed.map_err(io::Error::other))
        .map(|(_, outcome)| outcome)
        .map_err(|error| OciRunError(RunError::io("OCI artifact publication", error)))
}

pub(super) fn read<'a>(
    captured: &'a CapturedReleaseSource,
    root: &Path,
    candidate: &Path,
    target: LinuxOciTarget,
    archive: &Path,
    sdk_candidate: &Path,
) -> io::Result<OciOutcome<'a>> {
    let current = fingerprints(root)?;
    let bytes = read_regular(&root.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if metadata.files != current {
        return Err(invalid(
            "OCI artifact retained file bytes or inventory differ",
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
    let native = root.join("native/runtime");
    captured
        .check(&native.join("release-source"))
        .map_err(io::Error::other)?;
    let selected = Loaded::read(captured, candidate, target.server_target(), sdk_candidate)
        .map_err(io::Error::other)?;
    let saved = Loaded::read(
        captured,
        &native.join("server-candidate"),
        target.server_target(),
        &native.join("sdk-candidate"),
    )
    .map_err(io::Error::other)?;
    same_candidates(&selected, &saved)?;
    let digest = checksum(archive, 512 * 1024 * 1024)?;
    if checksum(&native.join("image.oci.tar"), 512 * 1024 * 1024)? != digest {
        return Err(invalid(
            "OCI artifact archive bytes differ from the selected input",
        ));
    }
    let selected_graph = selected.server.check_oci(archive)?;
    let saved_graph = saved.server.check_oci(&native.join("image.oci.tar"))?;
    if !graph_matches(&selected_graph, &saved_graph) {
        return Err(invalid(
            "OCI artifact graph differs from the selected archive",
        ));
    }
    check_outputs(captured, root, &saved, &metadata)?;
    Ok(OciOutcome {
        captured,
        projection: saved.server_projection,
        sdk_projection: saved.sdk_projection,
        target,
        version: saved.server.name().version().to_owned(),
        source_digest: saved.server.source_digest().to_owned(),
        server_checksum: saved.server.checksum().to_owned(),
        archive_checksum: digest,
        manifest_digest: saved_graph.manifest_digest().to_owned(),
        config_digest: saved_graph.config_digest().to_owned(),
        application_layer_digest: saved_graph.application_layer_digest().to_owned(),
        sdk_checksums: SdkPackage::ALL.map(|package| saved.sdk.checksum(package).to_owned()),
    })
}
