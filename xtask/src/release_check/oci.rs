use super::{candidate_run::RunError, CapturedReleaseSource, InputKey, LinuxOciTarget};
use crate::distribution::SdkPackage;
#[cfg(not(unix))]
use crate::process::ProcessError;
use std::path::Path;

#[cfg(unix)]
mod artifacts;

#[cfg(any(test, doctest))]
#[path = "oci_artifact_contracts.rs"]
mod artifact_contracts;

#[cfg(unix)]
use super::candidate_run::{checksum, combine, conclude_oci, Loaded, Paths, Workspace};
#[cfg(unix)]
use crate::{
    process::CommandSupervisor,
    server_distribution::{
        copy_archive, runtime_commands::RuntimeCommands, verify_loaded_oci_sdk_configured,
        OciSdkExecution, ServerOciContents,
    },
};
#[cfg(unix)]
use std::{
    io::{self, Write},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct OciOutcome<'a> {
    captured: &'a CapturedReleaseSource,
    projection: InputKey,
    sdk_projection: InputKey,
    target: LinuxOciTarget,
    version: String,
    source_digest: String,
    server_checksum: String,
    archive_checksum: String,
    manifest_digest: String,
    config_digest: String,
    application_layer_digest: String,
    sdk_checksums: [String; 3],
}

impl OciOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }
    pub const fn projection(&self) -> InputKey {
        self.projection
    }
    pub const fn sdk_projection(&self) -> InputKey {
        self.sdk_projection
    }
    pub const fn target(&self) -> LinuxOciTarget {
        self.target
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn server_checksum(&self) -> &str {
        &self.server_checksum
    }
    pub fn archive_checksum(&self) -> &str {
        &self.archive_checksum
    }
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }
    pub fn config_digest(&self) -> &str {
        &self.config_digest
    }
    pub fn application_layer_digest(&self) -> &str {
        &self.application_layer_digest
    }
    pub fn sdk_checksum(&self, package: SdkPackage) -> &str {
        let [core, api, client] = &self.sdk_checksums;
        match package {
            SdkPackage::Core => core,
            SdkPackage::Api => api,
            SdkPackage::Client => client,
        }
    }
}

pub struct OciRunError(RunError);
impl std::fmt::Display for OciRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}
impl std::fmt::Debug for OciRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}
impl std::error::Error for OciRunError {}

#[cfg(unix)]
struct OciRequest<'a> {
    candidate: &'a Path,
    target: LinuxOciTarget,
    archive: &'a Path,
    sdk_candidate: &'a Path,
    execution: &'a Path,
    consumer_execution: &'a Path,
    artifacts: &'a Path,
}

#[cfg(unix)]
fn graph_matches(first: &ServerOciContents, second: &ServerOciContents) -> bool {
    first.manifest_digest() == second.manifest_digest()
        && first.manifest_size() == second.manifest_size()
        && first.config_digest() == second.config_digest()
        && first.application_layer_digest() == second.application_layer_digest()
        && first.attestation_count() == second.attestation_count()
}

#[cfg(unix)]
fn verify_oci_with<'a>(
    captured: &'a CapturedReleaseSource,
    request: OciRequest<'_>,
    supervisor: CommandSupervisor,
    configure: impl FnMut(&mut Command, Duration) -> io::Result<()>,
) -> Result<OciOutcome<'a>, OciRunError> {
    verify_oci_recorded(
        captured,
        request,
        supervisor,
        configure,
        Retention::Diagnostic,
    )
}

#[cfg(unix)]
enum Retention<'a> {
    Diagnostic,
    Reusable(&'a Path),
}

#[cfg(unix)]
fn verify_oci_recorded<'a>(
    captured: &'a CapturedReleaseSource,
    request: OciRequest<'_>,
    mut supervisor: CommandSupervisor,
    mut configure: impl FnMut(&mut Command, Duration) -> io::Result<()>,
    retention: Retention<'_>,
) -> Result<OciOutcome<'a>, OciRunError> {
    let preflight = (|| {
        let loaded = Loaded::read(
            captured,
            request.candidate,
            request.target.server_target(),
            request.sdk_candidate,
        )?;
        let checked = loaded
            .server
            .check_oci(request.archive)
            .map_err(|error| RunError::io("OCI candidate graph", error))?;
        let digest = checksum(request.archive, 512 * 1024 * 1024)
            .map_err(|error| RunError::io("OCI candidate checksum", error))?;
        Ok((loaded, checked, digest))
    })();
    let (loaded, checked, digest) = match preflight {
        Ok(loaded) => loaded,
        Err(error) => {
            return combine(
                Err(error),
                supervisor
                    .finish()
                    .map_err(|error| RunError::process("OCI supervisor cleanup", error)),
            )
            .map_err(OciRunError)
        }
    };
    let workspace = match Workspace::create(
        request.execution,
        request.consumer_execution,
        request.artifacts,
    ) {
        Ok(workspace) => workspace,
        Err(error) => {
            return combine(
                Err(error),
                supervisor
                    .finish()
                    .map_err(|error| RunError::process("OCI supervisor cleanup", error)),
            )
            .map_err(OciRunError)
        }
    };
    let operation = (|| {
        captured
            .write(&workspace.source(), &workspace.state())
            .map_err(|error| RunError::source("OCI release source copy", error))?;
        copy_archive(request.archive, &workspace.archive(), 512 * 1024 * 1024)
            .map_err(|error| RunError::io("OCI owned archive copy", error))?;
        let copied_digest = checksum(&workspace.archive(), 512 * 1024 * 1024)
            .map_err(|error| RunError::io("OCI owned archive checksum", error))?;
        let copied = loaded
            .server
            .check_oci(&workspace.archive())
            .map_err(|error| RunError::io("OCI owned candidate graph", error))?;
        if copied_digest != digest || !graph_matches(&checked, &copied) {
            return Err(RunError::contract(
                "OCI owned archive binding",
                "OCI copied archive differs from the selected candidate",
            ));
        }
        let identity = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| RunError::io("OCI runtime clock", io::Error::other(error)))?
            .as_nanos();
        let identity = format!("{}-{identity}", std::process::id());
        let mut output = io::stdout();
        writeln!(
            output,
            "OCI verification artifacts: {}",
            workspace.artifacts().display()
        )
        .map_err(|error| RunError::io("OCI artifact output", error))?;
        output
            .flush()
            .map_err(|error| RunError::io("OCI artifact output", error))?;
        if let Retention::Reusable(root) = retention {
            artifacts::capture_tools(captured, root, &mut supervisor)
                .map_err(|error| RunError::io("OCI native tool capture", error))?;
        }
        let mut scoped = |command: &mut Command, budget| {
            if matches!(retention, Retention::Reusable(_)) {
                super::server::configure_tools(captured, command)?;
            }
            configure(command, budget)
        };
        let mut commands = RuntimeCommands::new(&mut supervisor, &mut scoped);
        verify_loaded_oci_sdk_configured(
            &loaded.server,
            &loaded.sdk,
            OciSdkExecution {
                identity: &identity,
                execution: request.execution,
                consumer_execution: &workspace.consumer(),
                artifacts: request.artifacts,
                archive: &workspace.archive(),
                checked: &copied,
            },
            &mut commands,
        )
        .map_err(|error| RunError::io("OCI server and SDK runtime", error))
    })();
    let context = operation.as_ref().ok();
    let preserved = match (context, &retention) {
        (Some(context), Retention::Reusable(_)) => artifacts::preserve_context(&request, context)
            .map_err(|error| RunError::io("OCI runtime input preservation", error)),
        (Some(_), Retention::Diagnostic)
        | (None, Retention::Diagnostic | Retention::Reusable(_)) => Ok(()),
    };
    let operation = combine(operation, preserved);
    let (operation, context) = match operation {
        Ok(context) => (Ok(()), Some(context)),
        Err(error) => (Err(error), None),
    };
    conclude_oci(operation, &loaded, captured, workspace, &digest, || {
        supervisor.finish()
    })
    .map_err(OciRunError)?;
    if let Retention::Reusable(root) = retention {
        loaded
            .preserve_paired(request.artifacts)
            .map_err(|error| OciRunError(RunError::io("OCI paired input preservation", error)))?;
        let context = context.ok_or_else(|| {
            OciRunError(RunError::contract(
                "OCI native context",
                "runtime context is missing",
            ))
        })?;
        artifacts::record(captured, &loaded, &request, &context, root)
            .map_err(|error| OciRunError(RunError::io("OCI native artifact validation", error)))?;
    }
    Ok(OciOutcome {
        captured,
        projection: loaded.server_projection,
        sdk_projection: loaded.sdk_projection,
        target: request.target,
        version: loaded.server.name().version().to_owned(),
        source_digest: loaded.server.source_digest().to_owned(),
        server_checksum: loaded.server.checksum().to_owned(),
        archive_checksum: digest,
        manifest_digest: checked.manifest_digest().to_owned(),
        config_digest: checked.config_digest().to_owned(),
        application_layer_digest: checked.application_layer_digest().to_owned(),
        sdk_checksums: SdkPackage::ALL.map(|package| loaded.sdk.checksum(package).to_owned()),
    })
}

impl CapturedReleaseSource {
    /// Checks one Linux OCI archive through native Docker and SDK execution.
    /// Returns after source validation and owned process and workspace cleanup.
    pub fn verify_oci(
        &self,
        candidate: &Path,
        target: LinuxOciTarget,
        archive: &Path,
        sdk_candidate: &Path,
    ) -> Result<OciOutcome<'_>, OciRunError> {
        #[cfg(unix)]
        {
            let paths = Paths::new(candidate, "oci").map_err(OciRunError)?;
            let supervisor = CommandSupervisor::new()
                .map_err(|error| OciRunError(RunError::process("OCI supervisor", error)))?;
            verify_oci_with(
                self,
                OciRequest {
                    candidate,
                    target,
                    archive,
                    sdk_candidate,
                    execution: &paths.execution,
                    consumer_execution: &paths.consumer_execution,
                    artifacts: &paths.artifacts,
                },
                supervisor,
                |_, _| Ok(()),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (candidate, target, archive, sdk_candidate);
            Err(OciRunError(RunError::process(
                "OCI supervisor",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }

    /// Publishes source-bound OCI native results after complete owned cleanup.
    pub fn verify_oci_artifact(
        &self,
        candidate: &Path,
        target: LinuxOciTarget,
        archive: &Path,
        sdk_candidate: &Path,
        published: &Path,
    ) -> Result<OciOutcome<'_>, OciRunError> {
        #[cfg(unix)]
        {
            artifacts::produce(self, candidate, target, archive, sdk_candidate, published)
        }
        #[cfg(not(unix))]
        {
            let _ = (candidate, target, archive, sdk_candidate, published);
            Err(OciRunError(RunError::process(
                "OCI artifact supervisor",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }

    /// Revalidates retained OCI native results with the selected candidate bytes.
    pub fn read_oci_artifact(
        &self,
        artifact: &Path,
        candidate: &Path,
        target: LinuxOciTarget,
        archive: &Path,
        sdk_candidate: &Path,
    ) -> Result<OciOutcome<'_>, OciRunError> {
        #[cfg(unix)]
        {
            artifacts::read(self, artifact, candidate, target, archive, sdk_candidate)
                .map_err(|error| OciRunError(RunError::io("OCI artifact reader", error)))
        }
        #[cfg(not(unix))]
        {
            let _ = (artifact, candidate, target, archive, sdk_candidate);
            Err(OciRunError(RunError::process(
                "OCI artifact reader",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }
}

#[cfg(any(test, doctest))]
#[path = "oci_outcome_contracts.rs"]
mod contracts;

#[cfg(test)]
#[path = "oci_tests.rs"]
mod tests;
