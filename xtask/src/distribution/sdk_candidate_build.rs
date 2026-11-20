use super::{
    check_dependency_paths_with, check_sdk_inputs_with, sdk, validate_sdk_archive_inputs,
    DependencyPathError, SdkArchiveError, SdkBuildError, SdkBuildPhase, SdkBuildPlan,
    SdkInputError, SnapshotError, SourceSnapshot,
};
use crate::{
    process::{run, ProcessError},
    regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot, RegularArchiveError},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum SdkCandidateBuildError {
    Io(std::io::Error),
    Snapshot(SnapshotError),
    Input(SdkInputError),
    Manifest(SdkArchiveError),
    Dependency(DependencyPathError),
    Build(SdkBuildError),
    Archive(RegularArchiveError),
    Version,
    Output,
    SourceChanged {
        operation: String,
        source: String,
    },
    FollowUp {
        operation: String,
        context: &'static str,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SdkCandidateBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Snapshot(error) => error.fmt(formatter),
            Self::Input(error) => error.fmt(formatter),
            Self::Manifest(error) => error.fmt(formatter),
            Self::Dependency(error) => error.fmt(formatter),
            Self::Build(error) => error.fmt(formatter),
            Self::Archive(error) => error.fmt(formatter),
            Self::Version => {
                formatter.write_str("SDK candidates require a portable workspace version")
            }
            Self::Output => {
                formatter.write_str("SDK candidates require bounded regular Cargo output")
            }
            Self::SourceChanged { operation, source } => {
                write!(formatter, "{operation}; SDK source check failed: {source}")
            }
            Self::FollowUp {
                operation,
                context,
                source,
            } => write!(formatter, "{operation}; {context}: {source}"),
        }
    }
}

impl std::error::Error for SdkCandidateBuildError {}

impl From<std::io::Error> for SdkCandidateBuildError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<SnapshotError> for SdkCandidateBuildError {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}
impl From<SdkInputError> for SdkCandidateBuildError {
    fn from(error: SdkInputError) -> Self {
        Self::Input(error)
    }
}
impl From<SdkArchiveError> for SdkCandidateBuildError {
    fn from(error: SdkArchiveError) -> Self {
        Self::Manifest(error)
    }
}
impl From<DependencyPathError> for SdkCandidateBuildError {
    fn from(error: DependencyPathError) -> Self {
        Self::Dependency(error)
    }
}
impl From<SdkBuildError> for SdkCandidateBuildError {
    fn from(error: SdkBuildError) -> Self {
        Self::Build(error)
    }
}
impl From<RegularArchiveError> for SdkCandidateBuildError {
    fn from(error: RegularArchiveError) -> Self {
        Self::Archive(error)
    }
}

pub struct CapturedSdk {
    source: SourceSnapshot,
    version: String,
}

fn captured(source: &SourceSnapshot, path: &str) -> Result<Vec<u8>, SdkInputError> {
    source
        .files()
        .find(|(file, _)| *file == Path::new(path))
        .map(|(_, bytes)| bytes.to_vec())
        .ok_or_else(|| SdkInputError::Input {
            path: path.to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        })
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}

fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, SdkCandidateBuildError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > limit {
        return Err(SdkCandidateBuildError::Output);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(SdkCandidateBuildError::Output);
    }
    Ok(bytes)
}

fn preserve_logs(state: &Path, destination: &Path) -> Result<(), std::io::Error> {
    for phase in SdkBuildPhase::ALL {
        let file = format!("{}.txt", phase.label());
        let path = state.join(&file);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let bytes = bounded_file(&path, 4 * 1024 * 1024).map_err(std::io::Error::other)?;
        write_new(&destination.join(file), &bytes)?;
    }
    Ok(())
}

fn fixture_workspace(original: &[u8]) -> Result<Vec<u8>, SdkCandidateBuildError> {
    let mut manifest = sdk::manifest("Cargo.toml", original)?;
    let Some(publish) = manifest
        .get_mut("workspace")
        .and_then(|value| value.get_mut("package"))
        .and_then(|value| value.get_mut("publish"))
    else {
        return Ok(original.to_vec());
    };
    if publish.as_bool() != Some(false) {
        return Err(
            std::io::Error::other("SDK workspace must disable registry publication").into(),
        );
    }
    *publish = toml::Value::Array(
        [toml::Value::String(
            super::sdk_build::FIXTURE_REGISTRY.to_owned(),
        )]
        .into(),
    );
    Ok(toml::to_string(&manifest)
        .map_err(std::io::Error::other)?
        .into_bytes())
}

fn collect(state: &Path, destination: &Path, version: &str) -> Result<(), SdkCandidateBuildError> {
    let mut checksums = String::new();
    for package in ["rusterisk-core", "rusterisk-api", "rusterisk-client"] {
        let root = format!("{package}-{version}");
        let name = format!("{root}.crate");
        let bytes = bounded_file(&state.join("target/package").join(&name), 16 * 1024 * 1024)?;
        let archive_root = ArchiveRoot::new(&root).map_err(|_| SdkCandidateBuildError::Version)?;
        read_regular_gzip(
            std::io::Cursor::new(&bytes),
            &archive_root,
            ArchiveLimits::SDK,
        )?;
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        checksums.push_str(&format!("{digest}  {name}\n"));
        write_new(&destination.join(name), &bytes)?;
    }
    write_new(&destination.join("SHA256SUMS"), checksums.as_bytes())?;
    Ok(())
}

fn follow_up(
    operation: Result<(), SdkCandidateBuildError>,
    result: Result<(), std::io::Error>,
    context: &'static str,
) -> Result<(), SdkCandidateBuildError> {
    match (operation, result) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(operation), Err(source)) => Err(SdkCandidateBuildError::FollowUp {
            operation: operation.to_string(),
            context,
            source,
        }),
    }
}

impl CapturedSdk {
    pub fn read(root: &Path) -> Result<Self, SdkCandidateBuildError> {
        let source = SourceSnapshot::read(root)?;
        check_sdk_inputs_with(|path| captured(&source, path))?;
        check_dependency_paths_with(|path| captured(&source, path))?;
        let manifest = sdk::manifest("Cargo.toml", &captured(&source, "Cargo.toml")?)?;
        let version = manifest
            .get("workspace")
            .and_then(|value| value.get("package"))
            .and_then(|value| value.get("version"))
            .and_then(toml::Value::as_str)
            .ok_or(SdkCandidateBuildError::Version)?;
        semver::Version::parse(version).map_err(|_| SdkCandidateBuildError::Version)?;
        ArchiveRoot::new(&format!("rusterisk-client-{version}"))
            .map_err(|_| SdkCandidateBuildError::Version)?;
        let client = captured(&source, "crates/client/Cargo.toml")?;
        let client = std::str::from_utf8(&client).map_err(|_| SdkCandidateBuildError::Version)?;
        validate_sdk_archive_inputs(client, version)?;
        source.check(root)?;
        Ok(Self {
            source,
            version: version.to_owned(),
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn source_digest(&self) -> String {
        self.source.digest()
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.source.files()
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn write(&self, source: &Path, state: &Path) -> Result<(), SdkCandidateBuildError> {
        self.source.write(source, state).map_err(Into::into)
    }

    pub fn build_with(
        &self,
        destination: &Path,
        perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> Result<(), SdkCandidateBuildError> {
        std::fs::create_dir(destination)?;
        self.source.write(
            &destination.join("source"),
            &destination.join("captured-browser"),
        )?;
        write_new(
            &destination.join("SOURCE.sha256"),
            format!("{}\n", self.source_digest()).as_bytes(),
        )?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "rusterisk-sdk-package-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&directory)?;
        let source = directory.join("source");
        let state = directory.join("state");
        let operation = (|| {
            self.source.write(&source, &state)?;
            let manifest = source.join("Cargo.toml");
            let original = captured(&self.source, "Cargo.toml")?;
            let fixture = fixture_workspace(&original)?;
            std::fs::write(&manifest, &fixture)?;
            let plan = SdkBuildPlan::new(&source, &state)?;
            let operation = plan
                .execute_with(perform)
                .map_err(SdkCandidateBuildError::Build);
            let checked = (|| {
                if bounded_file(&manifest, sdk::MAX_INPUT as u64)? != fixture {
                    return Err(SdkCandidateBuildError::Snapshot(SnapshotError::Changed));
                }
                std::fs::write(&manifest, &original)?;
                self.source
                    .check(&source)
                    .map_err(SdkCandidateBuildError::Snapshot)
            })();
            match (operation, checked) {
                (Ok(()), Ok(())) => collect(&state, destination, &self.version),
                (Err(error), Ok(())) => Err(error),
                (operation, Err(source)) => Err(SdkCandidateBuildError::SourceChanged {
                    operation: operation.err().map_or_else(
                        || "SDK packaging completed".to_owned(),
                        |error| error.to_string(),
                    ),
                    source: source.to_string(),
                }),
            }
        })();
        let operation = follow_up(
            operation,
            preserve_logs(&state, destination),
            "SDK log preservation failed",
        );
        follow_up(
            operation,
            std::fs::remove_dir_all(&directory),
            "SDK execution directory cleanup failed",
        )
    }

    pub fn build(&self, destination: &Path) -> Result<(), SdkCandidateBuildError> {
        self.build_with(destination, run)
    }
}

pub fn plan_sdk(root: &Path) -> Result<(), SdkCandidateBuildError> {
    let capture = CapturedSdk::read(root)?;
    println!(
        "SDK source={} version={}",
        capture.source_digest(),
        capture.version()
    );
    let preview = std::env::temp_dir().join("rusterisk-sdk-preview");
    let plan = SdkBuildPlan::new(&preview.join("source"), &preview.join("state"))?;
    for phase in SdkBuildPhase::ALL {
        let step = plan.step(phase);
        println!(
            "phase={} budget={}s cargo {}",
            phase.label(),
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::other(
            "SDK artifact directory must not redirect to another path",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path),
        Err(error) => Err(error),
    }
}

pub fn build_sdk_with(
    root: &Path,
    perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
) -> Result<PathBuf, SdkCandidateBuildError> {
    let capture = CapturedSdk::read(root)?;
    println!(
        "distribution: captured SDK source {}",
        capture.source_digest()
    );
    std::io::stdout().flush()?;
    let mut parent = root.to_owned();
    for component in [".cache", "distributions", "sdk"] {
        parent.push(component);
        ensure_directory(&parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let candidate = parent.join(format!("{}-{stamp}", std::process::id()));
    println!("SDK candidate: {}", candidate.display());
    capture.build_with(&candidate, perform)?;
    println!("SDK candidate packages created");
    Ok(candidate)
}

pub fn build_sdk(root: &Path) -> Result<PathBuf, SdkCandidateBuildError> {
    build_sdk_with(root, run)
}
