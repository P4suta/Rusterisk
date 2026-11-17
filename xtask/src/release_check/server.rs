use super::{candidate_run::RunError, CapturedReleaseSource, InputKey};
#[cfg(not(unix))]
use crate::process::ProcessError;
use crate::{distribution::SdkPackage, server_distribution::ServerTarget};
use std::path::Path;

#[cfg(unix)]
use super::candidate_run::{combine, conclude, Loaded, Paths, Workspace};
#[cfg(unix)]
use crate::{
    distribution::{PreparedSdkHttpConsumer, SdkHttpTarget},
    process::CommandSupervisor,
    server_distribution::{
        runtime_commands::RuntimeCommands, verify_loaded_server_configured, ServerExecutionMode,
    },
};
#[cfg(unix)]
use std::{
    io::{self, Write},
    process::Command,
    time::Duration,
};

pub struct ServerOutcome<'a> {
    captured: &'a CapturedReleaseSource,
    projection: InputKey,
    sdk_projection: InputKey,
    target: ServerTarget,
    version: String,
    checksum: String,
    source_digest: String,
    sdk_checksums: [String; 3],
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

#[cfg(unix)]
struct ServerRequest<'a> {
    candidate: &'a Path,
    target: ServerTarget,
    sdk_candidate: &'a Path,
    execution: &'a Path,
    consumer_execution: &'a Path,
    artifacts: &'a Path,
}

#[cfg(unix)]
fn native(request: &ServerRequest<'_>) -> Result<(), RunError> {
    match ServerExecutionMode::select(
        request.target,
        std::env::consts::OS,
        std::env::consts::ARCH,
        false,
    )
    .map_err(|error| RunError::io("native server target", error))?
    {
        ServerExecutionMode::Native => Ok(()),
        ServerExecutionMode::Rosetta => Err(RunError::contract(
            "native server target",
            "release verification requires a native host target",
        )),
    }
}

#[cfg(unix)]
fn verify_server_with<'a>(
    captured: &'a CapturedReleaseSource,
    request: ServerRequest<'_>,
    mut supervisor: CommandSupervisor,
    mut configure: impl FnMut(&mut Command, Duration) -> io::Result<()>,
) -> Result<ServerOutcome<'a>, ServerRunError> {
    let operation = (|| {
        native(&request)?;
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
        let mut commands = RuntimeCommands::new(&mut supervisor, &mut configure);
        let mut prepared = Some(
            PreparedSdkHttpConsumer::prepare_with(
                &loaded.sdk,
                &workspace.consumer(),
                &request.artifacts.join("consumer"),
                |command, budget| commands.run(command, budget),
            )
            .map_err(|error| RunError::consumer("server SDK preparation", error))?,
        );
        let operation = verify_loaded_server_configured(
            &loaded.server,
            &workspace.server(),
            &request.artifacts.join("server"),
            &mut commands,
            |service, address| {
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
                    .run_with(&target, |command, budget| service.run(command, budget))
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
    Ok(ServerOutcome {
        captured,
        projection: loaded.server_projection,
        sdk_projection: loaded.sdk_projection,
        target: request.target,
        version: loaded.server.name().version().to_owned(),
        checksum: loaded.server.checksum().to_owned(),
        source_digest: loaded.server.source_digest().to_owned(),
        sdk_checksums: SdkPackage::ALL.map(|package| loaded.sdk.checksum(package).to_owned()),
    })
}

impl CapturedReleaseSource {
    /// Checks distributed server bytes and their SDK consumer on the native host.
    /// Returns after supervised execution, source checks, and owned cleanup.
    pub fn verify_server(
        &self,
        candidate: &Path,
        target: ServerTarget,
        sdk_candidate: &Path,
    ) -> Result<ServerOutcome<'_>, ServerRunError> {
        #[cfg(unix)]
        {
            let paths = Paths::new(candidate, "server").map_err(ServerRunError)?;
            let supervisor = CommandSupervisor::new()
                .map_err(|error| ServerRunError(RunError::process("server supervisor", error)))?;
            verify_server_with(
                self,
                ServerRequest {
                    candidate,
                    target,
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
            let _ = (candidate, target, sdk_candidate);
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
