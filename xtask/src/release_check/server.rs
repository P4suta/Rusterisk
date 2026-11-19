use super::{candidate_run::RunError, CapturedReleaseSource, InputKey};
#[cfg(not(any(unix, windows)))]
use crate::process::ProcessError;
use crate::{
    distribution::SdkPackage,
    server_distribution::{ServerExecutionMode, ServerTarget},
};
#[cfg(any(unix, windows))]
use std::net::SocketAddr;
use std::path::Path;

#[cfg(any(unix, windows))]
mod artifacts;
#[cfg(unix)]
pub(super) use artifacts::{capture_tools, check_tools, configure_tools};

#[cfg(any(unix, windows))]
use super::candidate_run::{combine, conclude, Loaded, Paths, Workspace};
#[cfg(any(unix, windows))]
use crate::{
    distribution::{PreparedSdkHttpConsumer, SdkConsumerPlan, SdkHttpTarget},
    process::{CommandSupervisor, SupervisedCommand},
    server_distribution::{runtime_commands::RuntimeCommands, verify_loaded_server_configured},
};
#[cfg(any(unix, windows))]
use std::{
    io::{self, Write},
    num::NonZeroUsize,
    time::Duration,
};

pub struct ServerOutcome<'a> {
    captured: &'a CapturedReleaseSource,
    projection: InputKey,
    sdk_projection: InputKey,
    target: ServerTarget,
    execution: ServerExecutionMode,
    version: String,
    checksum: String,
    source_digest: String,
    sdk_checksums: [String; 3],
    #[cfg(any(unix, windows))]
    runtime_address: SocketAddr,
}

impl ServerOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }
    pub const fn projection(&self) -> InputKey {
        self.projection
    }
    pub const fn sdk_projection(&self) -> InputKey {
        self.sdk_projection
    }
    pub const fn target(&self) -> ServerTarget {
        self.target
    }
    pub const fn execution_mode(&self) -> ServerExecutionMode {
        self.execution
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
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

pub struct ServerRunError(RunError);
impl std::fmt::Display for ServerRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}
impl std::fmt::Debug for ServerRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}
impl std::error::Error for ServerRunError {}

#[cfg(any(unix, windows))]
struct ServerRequest<'a> {
    candidate: &'a Path,
    target: ServerTarget,
    mode: ServerExecutionMode,
    sdk_candidate: &'a Path,
    execution: &'a Path,
    consumer_execution: &'a Path,
    artifacts: &'a Path,
}

#[cfg(any(unix, windows))]
const fn consumer_jobs(target: ServerTarget) -> NonZeroUsize {
    match target {
        ServerTarget::WindowsX86_64 => NonZeroUsize::MIN,
        ServerTarget::LinuxX86_64
        | ServerTarget::LinuxArm64
        | ServerTarget::MacOsArm64
        | ServerTarget::MacOsX86_64 => SdkConsumerPlan::DEFAULT_JOBS,
    }
}

#[cfg(any(unix, windows))]
fn execution(request: &ServerRequest<'_>) -> Result<(), RunError> {
    let mode = ServerExecutionMode::select(
        request.target,
        std::env::consts::OS,
        std::env::consts::ARCH,
        request.mode == ServerExecutionMode::Rosetta,
    )
    .map_err(|error| RunError::io("server execution target", error))?;
    if mode == request.mode {
        Ok(())
    } else {
        Err(RunError::contract(
            "server execution target",
            "requested execution mode does not match the host and target",
        ))
    }
}

#[cfg(any(unix, windows))]
fn verify_server_with<'a>(
    captured: &'a CapturedReleaseSource,
    request: ServerRequest<'_>,
    supervisor: CommandSupervisor,
    configure: impl FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
) -> Result<ServerOutcome<'a>, ServerRunError> {
    verify_server_recorded(
        captured,
        request,
        supervisor,
        configure,
        Retention::Diagnostic,
    )
}

#[cfg(any(unix, windows))]
enum Retention<'a> {
    Diagnostic,
    Reusable(&'a Path),
}

#[cfg(any(unix, windows))]
fn verify_server_recorded<'a>(
    captured: &'a CapturedReleaseSource,
    request: ServerRequest<'_>,
    mut supervisor: CommandSupervisor,
    mut configure: impl FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
    retention: Retention<'_>,
) -> Result<ServerOutcome<'a>, ServerRunError> {
    let operation = (|| {
        execution(&request)?;
        Loaded::read(
            captured,
            request.candidate,
            request.target,
            request.sdk_candidate,
        )
    })();
    let loaded = match operation {
        Ok(loaded) => loaded,
        Err(error) => {
            return combine(
                Err(error),
                supervisor
                    .finish()
                    .map_err(|error| RunError::process("server supervisor cleanup", error)),
            )
            .map_err(ServerRunError)
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
                    .map_err(|error| RunError::process("server supervisor cleanup", error)),
            )
            .map_err(ServerRunError)
        }
    };
    let mut runtime_address = None;
    let operation = (|| {
        captured
            .write(&workspace.source(), &workspace.state())
            .map_err(|error| RunError::source("server release source copy", error))?;
        let mut output = io::stdout();
        writeln!(
            output,
            "Server verification artifacts: {}",
            workspace.artifacts().display()
        )
        .map_err(|error| RunError::io("server artifact output", error))?;
        output
            .flush()
            .map_err(|error| RunError::io("server artifact output", error))?;
        writeln!(
            output,
            "Server verification target={} execution={}",
            request.target.triple(),
            request.mode.label(),
        )
        .map_err(|error| RunError::io("server execution output", error))?;
        if let Retention::Reusable(root) = retention {
            artifacts::capture_tools(captured, root, &mut supervisor)
                .map_err(|error| RunError::io("native tool capture", error))?;
        }
        let mut configure_bound = |command: &mut SupervisedCommand, budget: Duration| {
            if let Retention::Reusable(_) = retention {
                artifacts::configure_owned_tools(captured, command)?;
            }
            configure(command, budget)
        };
        let mut commands = RuntimeCommands::new(&mut supervisor, &mut configure_bound);
        let mut prepared = Some(
            PreparedSdkHttpConsumer::prepare_with_jobs(
                &loaded.sdk,
                &workspace.consumer(),
                &request.artifacts.join("consumer"),
                consumer_jobs(request.target),
                |command, budget| commands.run_owned(command, budget),
            )
            .map_err(|error| RunError::consumer("server SDK preparation", error))?,
        );
        let operation = verify_loaded_server_configured(
            &loaded.server,
            &workspace.server(),
            &request.artifacts.join("server"),
            &mut commands,
            |service, address| {
                runtime_address = Some(address);
                let target = SdkHttpTarget::new(
                    address,
                    "api",
                    "change-at-install",
                    loaded.sdk.version(),
                    "release-server",
                )
                .map_err(io::Error::other)?;
                prepared
                    .take()
                    .ok_or_else(|| {
                        io::Error::other("prepared server SDK consumer ownership ended")
                    })?
                    .run_with(&target, |command, budget| {
                        service.run_owned(command, budget)
                    })
                    .map(|_| ())
                    .map_err(io::Error::other)
            },
        )
        .map_err(|error| RunError::io("native server and SDK runtime", error));
        let cleanup = match prepared {
            Some(prepared) => prepared
                .close()
                .map_err(|error| RunError::consumer("server SDK consumer cleanup", error)),
            None => Ok(()),
        };
        combine(operation, cleanup)
    })();
    conclude(operation, &loaded, captured, workspace, || {
        supervisor.finish()
    })
    .map_err(ServerRunError)?;
    let outcome = ServerOutcome {
        captured,
        projection: loaded.server_projection,
        sdk_projection: loaded.sdk_projection,
        target: request.target,
        execution: request.mode,
        version: loaded.server.name().version().to_owned(),
        checksum: loaded.server.checksum().to_owned(),
        source_digest: loaded.server.source_digest().to_owned(),
        sdk_checksums: SdkPackage::ALL.map(|package| loaded.sdk.checksum(package).to_owned()),
        runtime_address: runtime_address.ok_or_else(|| {
            ServerRunError(RunError::contract(
                "server runtime observation",
                "successful server verification has no bound listener",
            ))
        })?,
    };
    if let Retention::Reusable(root) = retention {
        loaded
            .preserve_paired(request.artifacts)
            .and_then(|()| artifacts::record(captured, root, &request, &loaded, &outcome))
            .map_err(|error| ServerRunError(RunError::io("native artifact preservation", error)))?;
    }
    Ok(outcome)
}

impl CapturedReleaseSource {
    pub fn verify_server_artifact(
        &self,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
        mode: ServerExecutionMode,
        published: &Path,
    ) -> Result<ServerOutcome<'_>, ServerRunError> {
        #[cfg(any(unix, windows))]
        {
            artifacts::produce(self, candidate, target, sdk_candidate, mode, published)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (candidate, target, sdk_candidate, mode, published);
            Err(ServerRunError(RunError::process(
                "native artifact producer",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }

    pub fn read_server_artifact(
        &self,
        artifact: &Path,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
        mode: ServerExecutionMode,
    ) -> Result<ServerOutcome<'_>, ServerRunError> {
        #[cfg(any(unix, windows))]
        {
            artifacts::read(self, artifact, candidate, target, sdk_candidate, mode)
                .map_err(|error| ServerRunError(RunError::io("native artifact validation", error)))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (artifact, candidate, target, sdk_candidate, mode);
            Err(ServerRunError(RunError::process(
                "native artifact reader",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }

    /// Checks distributed server bytes and their SDK consumer on the native host.
    /// Returns after supervised execution, source checks, and owned cleanup.
    pub fn verify_server(
        &self,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
    ) -> Result<ServerOutcome<'_>, ServerRunError> {
        self.verify_server_with_execution(
            candidate,
            target,
            sdk_candidate,
            ServerExecutionMode::Native,
        )
    }

    /// Checks distributed server bytes under an explicit execution mode.
    pub fn verify_server_with_execution(
        &self,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
        mode: ServerExecutionMode,
    ) -> Result<ServerOutcome<'_>, ServerRunError> {
        #[cfg(any(unix, windows))]
        {
            let paths = Paths::new(candidate, "server").map_err(ServerRunError)?;
            let supervisor = CommandSupervisor::new()
                .map_err(|error| ServerRunError(RunError::process("server supervisor", error)))?;
            verify_server_with(
                self,
                ServerRequest {
                    candidate,
                    target,
                    mode,
                    sdk_candidate,
                    execution: &paths.execution,
                    consumer_execution: &paths.consumer_execution,
                    artifacts: &paths.artifacts,
                },
                supervisor,
                |_, _| Ok(()),
            )
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (candidate, target, sdk_candidate, mode);
            Err(ServerRunError(RunError::process(
                "server supervisor",
                ProcessError::UnsupportedSupervision,
            )))
        }
    }
}

#[cfg(any(test, doctest))]
#[path = "server_outcome_contracts.rs"]
mod contracts;

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
