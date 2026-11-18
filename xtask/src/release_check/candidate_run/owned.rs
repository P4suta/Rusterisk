use super::{combine, RunError};
use crate::{
    distribution::{CapturedSdk, SdkContents},
    release_check::{CapturedReleaseSource, InputKey, SourceInputs},
    server_distribution::{CapturedServer, ServerCandidate, ServerTarget},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(in crate::release_check) struct Loaded {
    pub(in crate::release_check) server: ServerCandidate,
    pub(in crate::release_check) sdk: SdkContents,
    pub(in crate::release_check) server_projection: InputKey,
    pub(in crate::release_check) sdk_projection: InputKey,
    server_source: CapturedServer,
    sdk_source: CapturedSdk,
    server_root: PathBuf,
    sdk_root: PathBuf,
}

impl Loaded {
    pub(in crate::release_check) fn read(
        captured: &CapturedReleaseSource,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
    ) -> Result<Self, RunError> {
        let (server_source, server) = ServerCandidate::read_paired(candidate, target)
            .map_err(|error| RunError::io("server candidate bytes", error))?;
        let release = captured
            .inputs()
            .map_err(|error| RunError::inputs("release inputs", error))?;
        let server_projection = SourceInputs::new(server_source.files())
            .and_then(|projection| release.bind(&projection))
            .map_err(|error| RunError::inputs("server source binding", error))?;
        let (sdk_source, sdk) = SdkContents::read_paired(sdk_candidate)
            .map_err(|error| RunError::source("SDK candidate bytes", error))?;
        let sdk_projection = SourceInputs::new(sdk_source.files())
            .and_then(|projection| release.bind(&projection))
            .map_err(|error| RunError::inputs("SDK source binding", error))?;
        if server.name().version() != sdk.version() {
            return Err(RunError::contract(
                "candidate versions",
                "server and SDK candidate versions differ",
            ));
        }
        Ok(Self {
            server,
            sdk,
            server_projection,
            sdk_projection,
            server_source,
            sdk_source,
            server_root: candidate.to_owned(),
            sdk_root: sdk_candidate.to_owned(),
        })
    }

    pub(in crate::release_check) fn check(
        &self,
        captured: &CapturedReleaseSource,
        source: &Path,
    ) -> Result<(), RunError> {
        captured
            .check(source)
            .map_err(|error| RunError::source("release source after execution", error))?;
        self.server_source
            .check(&self.server_root.join("source"))
            .map_err(|error| RunError::source("server source after execution", error))?;
        let sdk = CapturedSdk::read(&self.sdk_root.join("source"))
            .map_err(|error| RunError::source("SDK source after execution", error))?;
        if !self.sdk_source.files().eq(sdk.files()) {
            return Err(RunError::contract(
                "SDK source after execution",
                "SDK candidate source changed",
            ));
        }
        Ok(())
    }

    pub(in crate::release_check) fn preserve(
        &self,
        captured: &CapturedReleaseSource,
        artifacts: &Path,
    ) -> Result<(), RunError> {
        let directory = artifacts.join("server-candidate");
        fs::create_dir(&directory)
            .map_err(|error| RunError::io("server candidate preservation", error))?;
        let name = self.server.name().file_name();
        let archive = self.server_root.join(name);
        copy_checked(
            &archive,
            &directory.join(name),
            self.server.checksum(),
            272 * 1024 * 1024,
        )
        .map_err(|error| RunError::io("server archive preservation", error))?;
        write(
            &directory.join("SHA256SUMS"),
            format!("{}  {name}\n", self.server.checksum()).as_bytes(),
        )
        .map_err(|error| RunError::io("server checksum preservation", error))?;
        write(
            &directory.join("SOURCE.sha256"),
            format!("{}\n", self.server.source_digest()).as_bytes(),
        )
        .map_err(|error| RunError::io("server source identity preservation", error))?;
        captured
            .write(
                &artifacts.join("release-source"),
                &artifacts.join("captured-browser"),
            )
            .map_err(|error| RunError::source("release source preservation", error))
    }

    pub(in crate::release_check) fn preserve_paired(&self, artifacts: &Path) -> io::Result<()> {
        let server = artifacts.join("server-candidate");
        self.server_source
            .write(&server.join("source"), &server.join("captured-browser"))?;
        for name in ["fetch.txt", "build.txt"] {
            let bytes = crate::release_check::native_artifacts::read_regular(
                &self.server_root.join(name),
                4 * 1024 * 1024,
            )?;
            write(&server.join(name), &bytes)?;
        }
        let sdk = artifacts.join("sdk-candidate");
        fs::create_dir(&sdk)?;
        self.sdk_source
            .write(&sdk.join("source"), &sdk.join("captured-browser"))
            .map_err(io::Error::other)?;
        let mut sums = String::new();
        for package in crate::distribution::SdkPackage::ALL {
            let name = format!("{}-{}.crate", package.name(), self.sdk.version());
            copy_checked(
                &self.sdk_root.join(&name),
                &sdk.join(&name),
                self.sdk.checksum(package),
                16 * 1024 * 1024,
            )?;
            sums.push_str(&format!("{}  {name}\n", self.sdk.checksum(package)));
        }
        write(&sdk.join("SHA256SUMS"), sums.as_bytes())?;
        write(
            &sdk.join("SOURCE.sha256"),
            format!("{}\n", self.sdk.source_digest()).as_bytes(),
        )?;
        Ok(())
    }
}

pub(in crate::release_check) fn checksum(path: &Path, maximum: u64) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(io::Error::other(
            "candidate requires a bounded regular archive",
        ));
    }
    let mut input = fs::File::open(path)?;
    if !input.metadata()?.is_file() || input.metadata()?.len() != metadata.len() {
        return Err(io::Error::other("candidate archive changed before reading"));
    }
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 65_536];
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        total = total
            .checked_add(size as u64)
            .filter(|total| *total <= maximum)
            .ok_or_else(|| io::Error::other("candidate archive exceeded its byte limit"))?;
        digest.update(&buffer[..size]);
    }
    if total != metadata.len() {
        return Err(io::Error::other("candidate archive changed while reading"));
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(in crate::release_check) fn copy_checked(
    source: &Path,
    destination: &Path,
    expected: &str,
    maximum: u64,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(io::Error::other(
            "candidate requires a bounded regular archive",
        ));
    }
    let mut input = fs::File::open(source)?;
    if !input.metadata()?.is_file() || input.metadata()?.len() != metadata.len() {
        return Err(io::Error::other("candidate archive changed before copying"));
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let copied = io::copy(&mut Read::take(&mut input, maximum + 1), &mut output)?;
    output.flush()?;
    if copied != metadata.len() || copied > maximum || checksum(destination, maximum)? != expected {
        return Err(io::Error::other(
            "candidate archive differs from the verified bytes",
        ));
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)
}

pub(in crate::release_check) struct Paths {
    pub(in crate::release_check) execution: PathBuf,
    pub(in crate::release_check) consumer_execution: PathBuf,
    pub(in crate::release_check) artifacts: PathBuf,
}

impl Paths {
    pub(in crate::release_check) fn new(candidate: &Path, kind: &str) -> Result<Self, RunError> {
        let candidate = fs::canonicalize(candidate)
            .map_err(|error| RunError::io("candidate directory", error))?;
        let parent = candidate.parent().ok_or_else(|| {
            RunError::contract("candidate parent", "candidate requires a physical parent")
        })?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| RunError::io("candidate execution clock", io::Error::other(error)))?
            .as_nanos();
        let identity = format!("{}-{stamp}", std::process::id());
        let temporary = fs::canonicalize(std::env::temp_dir())
            .map_err(|error| RunError::io("candidate temporary root", error))?;
        Ok(Self {
            execution: temporary.join(format!("rusterisk-{kind}-{identity}")),
            consumer_execution: temporary.join(format!("rusterisk-{kind}-consumer-{identity}")),
            artifacts: parent.join(format!("{kind}-verification-{identity}")),
        })
    }
}

pub(in crate::release_check) struct Workspace {
    execution: PathBuf,
    consumer: PathBuf,
    artifacts: PathBuf,
    execution_owned: bool,
    consumer_owned: bool,
}

impl Workspace {
    pub(in crate::release_check) fn create(
        execution: &Path,
        consumer: &Path,
        artifacts: &Path,
    ) -> Result<Self, RunError> {
        for path in [execution, consumer, artifacts] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
            {
                return Err(RunError::contract(
                    "candidate workspace paths",
                    "candidate workspaces require separate physical absolute paths",
                ));
            }
            let parent = path.parent().ok_or_else(|| {
                RunError::contract(
                    "candidate workspace parent",
                    "candidate workspace requires a parent",
                )
            })?;
            if fs::canonicalize(parent)
                .map_err(|error| RunError::io("candidate workspace parent", error))?
                != parent
            {
                return Err(RunError::contract(
                    "candidate workspace parent",
                    "candidate workspace requires a physical parent",
                ));
            }
        }
        for (index, first) in [execution, consumer, artifacts].iter().enumerate() {
            if [execution, consumer, artifacts]
                .iter()
                .skip(index + 1)
                .any(|second| first.starts_with(second) || second.starts_with(first))
            {
                return Err(RunError::contract(
                    "candidate workspace paths",
                    "candidate workspaces must not overlap",
                ));
            }
        }
        let mut workspace = Self {
            execution: execution.to_owned(),
            consumer: consumer.to_owned(),
            artifacts: artifacts.to_owned(),
            execution_owned: false,
            consumer_owned: false,
        };
        let operation = (|| {
            fs::create_dir(execution)?;
            workspace.execution_owned = true;
            fs::create_dir(consumer)?;
            workspace.consumer_owned = true;
            fs::create_dir(artifacts)?;
            Ok(())
        })();
        match operation {
            Ok(()) => Ok(workspace),
            Err(error) => combine(
                Err(RunError::io("candidate workspace creation", error)),
                workspace.close(),
            ),
        }
    }

    pub(in crate::release_check) fn source(&self) -> PathBuf {
        self.execution.join("source")
    }
    pub(in crate::release_check) fn state(&self) -> PathBuf {
        self.execution.join("state")
    }
    pub(in crate::release_check) fn server(&self) -> PathBuf {
        self.execution.join("server")
    }
    pub(in crate::release_check) fn consumer(&self) -> PathBuf {
        self.consumer.join("consumer")
    }
    #[cfg(unix)]
    pub(in crate::release_check) fn archive(&self) -> PathBuf {
        self.execution.join("image.oci.tar")
    }

    pub(in crate::release_check) fn close(self) -> Result<(), RunError> {
        let mut result = Ok(());
        for (owned, path) in [
            (self.consumer_owned, &self.consumer),
            (self.execution_owned, &self.execution),
        ] {
            if owned {
                result = combine(
                    result,
                    fs::remove_dir_all(path)
                        .map_err(|error| RunError::io("candidate workspace cleanup", error)),
                );
            }
        }
        result
    }

    pub(in crate::release_check) fn artifacts(&self) -> &Path {
        &self.artifacts
    }
}
