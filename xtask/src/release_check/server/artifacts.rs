use super::{
    verify_server_recorded, CapturedReleaseSource, CommandSupervisor, Loaded, Paths, Retention,
    RunError, ServerExecutionMode, ServerOutcome, ServerRequest, ServerRunError, ServerTarget,
};
use crate::{
    distribution::{check_sdk_http_results_recorded, SdkPackage},
    recorded_path::{RecordedPath, RecordedPathFormat},
    release_check::native_artifacts::{
        fingerprints, read_regular, FileFingerprint, NativeArtifactPublication,
        PublishedNativeArtifact,
    },
    server_distribution::check_native_server_outputs,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};

mod tools;
pub(in crate::release_check) use tools::{
    capture as capture_tools, configure_owned as configure_owned_tools,
};
#[cfg(unix)]
pub(in crate::release_check) use tools::{check as check_tools, configure as configure_tools};

const METADATA: &str = "native-check.toml";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    format: u32,
    kind: String,
    target: String,
    execution: String,
    os: String,
    architecture: String,
    server_root: PathBuf,
    consumer_root: PathBuf,
    http: SocketAddr,
    files: Vec<FileFingerprint>,
}

struct OriginalPaths {
    server: RecordedPath,
    consumer: RecordedPath,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn original_path(format: RecordedPathFormat, path: &Path) -> io::Result<RecordedPath> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("original native path is not UTF-8"))?;
    if matches!(format, RecordedPathFormat::Unix)
        && (text.contains('\\') || text.split('/').any(|part| part == "."))
    {
        return Err(invalid(
            "original native paths require bounded absolute components",
        ));
    }
    format.parse(path, 4096)
}

impl OriginalPaths {
    fn new(format: RecordedPathFormat, server: &Path, consumer: &Path) -> io::Result<Self> {
        let server = original_path(format, server)?;
        let consumer = original_path(format, consumer)?;
        if server.file_name() != Some("server")
            || consumer.file_name() != Some("consumer")
            || server.starts_with(&consumer)
            || consumer.starts_with(&server)
        {
            return Err(invalid(
                "original native paths differ from the owned workspace profile",
            ));
        }
        Ok(Self { server, consumer })
    }
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

fn host(metadata: &Metadata, target: ServerTarget, mode: ServerExecutionMode) -> io::Result<()> {
    if metadata.format != FORMAT
        || metadata.kind != "server"
        || metadata.target != target.triple()
        || metadata.execution != mode.label()
    {
        return Err(invalid(
            "native artifact format, gate, target, or execution profile differs",
        ));
    }
    let selected = ServerExecutionMode::select(
        target,
        &metadata.os,
        &metadata.architecture,
        mode == ServerExecutionMode::Rosetta,
    )?;
    if selected != mode {
        return Err(invalid(
            "native artifact host does not support the selected execution profile",
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
    let format = match metadata.os.as_str() {
        "macos" | "linux" => RecordedPathFormat::Unix,
        "windows" => RecordedPathFormat::Windows,
        _ => return Err(invalid("unsupported recorded server host")),
    };
    let paths = OriginalPaths::new(format, &metadata.server_root, &metadata.consumer_root)?;
    tools::check(
        captured,
        &root.join("tools"),
        &metadata.os,
        &metadata.architecture,
    )?;
    let server_outputs = root.join("native/server");
    let binary = loaded.server.name().target().binary_path();
    check_native_server_outputs(
        &loaded.server,
        &server_outputs,
        &metadata.os,
        metadata.http,
        &paths.server.join(binary),
    )?;
    if metadata.os == "macos" {
        let output = read_regular(&server_outputs.join("dependencies.stdout.txt"), 65_536)?;
        let text = std::str::from_utf8(&output).map_err(io::Error::other)?;
        let original = metadata
            .server_root
            .to_str()
            .ok_or_else(|| invalid("missing original executable path"))?;
        let executable = format!("{}/{binary}:", original.trim_end_matches('/'));
        if text.lines().next() != Some(executable.as_str()) {
            return Err(invalid(
                "native dependency output belongs to another original executable",
            ));
        }
    }
    check_sdk_http_results_recorded(
        &loaded.sdk,
        &root.join("native/consumer"),
        format,
        &paths.consumer,
    )
    .map_err(io::Error::other)
}

pub(super) fn record(
    captured: &CapturedReleaseSource,
    root: &Path,
    request: &ServerRequest<'_>,
    loaded: &Loaded,
    outcome: &ServerOutcome<'_>,
) -> io::Result<()> {
    let metadata = Metadata {
        format: FORMAT,
        kind: "server".to_owned(),
        target: outcome.target().triple().to_owned(),
        execution: outcome.execution_mode().label().to_owned(),
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        server_root: request.execution.join("server"),
        consumer_root: request.consumer_execution.join("consumer"),
        http: outcome.runtime_address,
        files: fingerprints(root)?,
    };
    host(&metadata, outcome.target(), outcome.execution_mode())?;
    captured
        .check(&root.join("native/release-source"))
        .map_err(io::Error::other)?;
    check_outputs(captured, root, loaded, &metadata)?;
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
    target: ServerTarget,
    sdk_candidate: &Path,
    mode: ServerExecutionMode,
    published: &Path,
) -> Result<ServerOutcome<'a>, ServerRunError> {
    let paths = Paths::new(candidate, "server-artifact").map_err(ServerRunError)?;
    let parent = published.parent().ok_or_else(|| {
        ServerRunError(RunError::contract(
            "native artifact destination",
            "publication destination has no parent",
        ))
    })?;
    let identity = paths.artifacts.file_name().ok_or_else(|| {
        ServerRunError(RunError::contract(
            "native artifact staging",
            "staging identity is missing",
        ))
    })?;
    let staging = parent.join(identity);
    for input in [candidate, sdk_candidate] {
        let input = fs::canonicalize(input)
            .map_err(|error| ServerRunError(RunError::io("native artifact input", error)))?;
        if staging.starts_with(&input) || published.starts_with(&input) {
            return Err(ServerRunError(RunError::contract(
                "native artifact destination",
                "native artifact destination overlaps candidate inputs",
            )));
        }
    }
    let publication = NativeArtifactPublication::create(&staging, published)
        .map_err(|error| ServerRunError(RunError::io("native artifact destination", error)))?;
    let native = publication.staging().join("native");
    let supervisor = CommandSupervisor::new()
        .map_err(|error| ServerRunError(RunError::process("native artifact supervisor", error)))?;
    let completed = verify_server_recorded(
        captured,
        ServerRequest {
            candidate,
            target,
            mode,
            sdk_candidate,
            execution: &paths.execution,
            consumer_execution: &paths.consumer_execution,
            artifacts: &native,
        },
        supervisor,
        |_, _| Ok(()),
        Retention::Reusable(publication.staging()),
    );
    publication
        .publish_completed(completed.map_err(io::Error::other))
        .map(|(_, outcome)| outcome)
        .map_err(|error| ServerRunError(RunError::io("native artifact publication", error)))
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
            "native artifact candidate bytes or source projections differ",
        ));
    }
    Ok(())
}

pub(super) fn read<'a>(
    captured: &'a CapturedReleaseSource,
    root: &Path,
    candidate: &Path,
    target: ServerTarget,
    sdk_candidate: &Path,
    mode: ServerExecutionMode,
) -> io::Result<ServerOutcome<'a>> {
    let current = fingerprints(root)?;
    let bytes = read_regular(&root.join(METADATA), 8 * 1024 * 1024)?;
    let metadata: Metadata = toml::from_str(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    host(&metadata, target, mode)?;
    let current: Vec<_> = current
        .into_iter()
        .filter(|file| file.path() != METADATA)
        .collect();
    if metadata.files != current {
        return Err(invalid(
            "native artifact retained file bytes or inventory differ",
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
    let selected =
        Loaded::read(captured, candidate, target, sdk_candidate).map_err(io::Error::other)?;
    let saved = Loaded::read(
        captured,
        &root.join("native/server-candidate"),
        target,
        &root.join("native/sdk-candidate"),
    )
    .map_err(io::Error::other)?;
    same_candidates(&selected, &saved)?;
    check_outputs(captured, root, &saved, &metadata)?;
    Ok(ServerOutcome {
        captured,
        projection: saved.server_projection,
        sdk_projection: saved.sdk_projection,
        target,
        execution: mode,
        version: saved.server.name().version().to_owned(),
        checksum: saved.server.checksum().to_owned(),
        source_digest: saved.server.source_digest().to_owned(),
        sdk_checksums: SdkPackage::ALL.map(|package| saved.sdk.checksum(package).to_owned()),
        runtime_address: metadata.http,
    })
}
