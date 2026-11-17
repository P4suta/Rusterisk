use super::CapturedReleaseSource;
use crate::process::{CommandSupervisor, ProcessError};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
enum NativeGate {
    Fast,
    Security,
}

impl NativeGate {
    const fn entry(self) -> &'static str {
        match self {
            Self::Fast => "check",
            Self::Security => "security",
        }
    }

    const fn budget(self) -> Duration {
        match self {
            Self::Fast => Duration::from_secs(1800),
            Self::Security => Duration::from_secs(600),
        }
    }
}

struct NativePlan {
    gate: NativeGate,
}

impl NativePlan {
    const fn new(gate: NativeGate) -> Self {
        Self { gate }
    }
}

/// A fast outcome requires native execution and complete cleanup.
///
/// ```compile_fail
/// use xtask::release_check::{FastOutcome, RequiredGate};
/// fn planned<'a>() -> FastOutcome<'a> { RequiredGate::Fast }
/// ```
///
/// ```compile_fail
/// use xtask::release_check::{CapturedReleaseSource, FastOutcome};
/// fn fabricated(captured: &CapturedReleaseSource) -> FastOutcome<'_> {
///     FastOutcome { captured }
/// }
/// ```
///
/// ```compile_fail
/// use xtask::release_check::{CapturedReleaseSource, FastOutcome};
/// fn synthetic(captured: &CapturedReleaseSource) -> FastOutcome<'_> {
///     captured.verify_fast_with(|_: &mut std::process::Command, _: std::time::Duration| Ok::<(), std::io::Error>(()))
/// }
/// ```
pub struct FastOutcome<'a> {
    captured: &'a CapturedReleaseSource,
}

impl FastOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }
}

/// A security outcome requires native execution and complete cleanup.
///
/// ```compile_fail
/// use xtask::release_check::{RequiredGate, SecurityOutcome};
/// fn planned<'a>() -> SecurityOutcome<'a> { RequiredGate::Security }
/// ```
///
/// ```compile_fail
/// use xtask::release_check::{CapturedReleaseSource, SecurityOutcome};
/// fn fabricated(captured: &CapturedReleaseSource) -> SecurityOutcome<'_> {
///     SecurityOutcome { captured }
/// }
/// ```
///
/// ```compile_fail
/// use xtask::release_check::{CapturedReleaseSource, SecurityOutcome};
/// fn synthetic(captured: &CapturedReleaseSource) -> SecurityOutcome<'_> {
///     captured.verify_security_with(|_: &mut std::process::Command, _: std::time::Duration| Ok::<(), std::io::Error>(()))
/// }
/// ```
pub struct SecurityOutcome<'a> {
    captured: &'a CapturedReleaseSource,
}

impl SecurityOutcome<'_> {
    pub fn inputs(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.captured.files()
    }
}

enum NativeOutcome<'a> {
    Fast(FastOutcome<'a>),
    Security(SecurityOutcome<'a>),
}

enum Failure {
    Io(io::Error),
    Source(String),
    Process(ProcessError),
    Contract(&'static str),
}

/// Keeps native execution and follow-up failures in their observed order.
pub struct NativeGateError {
    preceding: Option<String>,
    stage: &'static str,
    failure: Failure,
}

impl NativeGateError {
    fn new(stage: &'static str, failure: Failure) -> Self {
        Self {
            preceding: None,
            stage,
            failure,
        }
    }
}

impl std::fmt::Debug for NativeGateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for NativeGateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(preceding) = &self.preceding {
            write!(formatter, "{preceding}; ")?;
        }
        write!(formatter, "{}: ", self.stage)?;
        match &self.failure {
            Failure::Io(error) => error.fmt(formatter),
            Failure::Source(diagnostic) => formatter.write_str(diagnostic),
            Failure::Process(error) => error.fmt(formatter),
            Failure::Contract(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for NativeGateError {}

fn combine<T>(
    operation: Result<T, NativeGateError>,
    follow_up: Result<(), NativeGateError>,
) -> Result<T, NativeGateError> {
    match (operation, follow_up) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(mut follow_up)) => {
            follow_up.preceding = Some(match follow_up.preceding.take() {
                Some(preceding) => format!("{operation}; {preceding}"),
                None => operation.to_string(),
            });
            Err(follow_up)
        }
    }
}

trait NativePort {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError>;
    fn finish(self) -> Result<(), ProcessError>;
}

impl NativePort for CommandSupervisor {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        CommandSupervisor::run(self, command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        CommandSupervisor::finish(self)
    }
}

struct Workspace {
    root: PathBuf,
    source: PathBuf,
    state: PathBuf,
}

impl Workspace {
    fn create(root: &Path) -> Result<Self, NativeGateError> {
        fs::create_dir(root)
            .map_err(|error| NativeGateError::new("native workspace", Failure::Io(error)))?;
        Ok(Self {
            root: root.to_owned(),
            source: root.join("source"),
            state: root.join("state"),
        })
    }

    fn command(&self, plan: &NativePlan) -> Result<Command, NativeGateError> {
        let source = self
            .source
            .canonicalize()
            .map_err(|error| NativeGateError::new("native source path", Failure::Io(error)))?;
        let config = self.state.join("mise-global.toml");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config)
            .map_err(|error| NativeGateError::new("native mise profile", Failure::Io(error)))?;
        let mut command = Command::new("mise");
        command
            .current_dir(&source)
            .args(["x", "--", "just", plan.gate.entry()])
            .env("MISE_TRUSTED_CONFIG_PATHS", &source)
            .env("MISE_CEILING_PATHS", &source)
            .env("MISE_GLOBAL_CONFIG_FILE", &config)
            .env("MISE_SYSTEM_CONFIG_FILE", &config)
            .env_remove("MISE_CONFIG_FILE")
            .env_remove("MISE_ENV")
            .env_remove("CARGO_TARGET_DIR")
            .env("CARGO_BUILD_JOBS", "1")
            .env("RUST_TEST_THREADS", "1");
        match plan.gate {
            NativeGate::Fast => {}
            NativeGate::Security => {
                let temporary = self.state.join("temporary");
                fs::create_dir(&temporary).map_err(|error| {
                    NativeGateError::new("native temporary directory", Failure::Io(error))
                })?;
                let temporary = temporary.canonicalize().map_err(|error| {
                    NativeGateError::new("native temporary path", Failure::Io(error))
                })?;
                command
                    .env("TMPDIR", &temporary)
                    .env("TMP", &temporary)
                    .env("TEMP", &temporary);
            }
        }
        Ok(command)
    }
}

fn verify_with<'a>(
    captured: &'a CapturedReleaseSource,
    plan: NativePlan,
    directory: &Path,
    mut port: impl NativePort,
) -> Result<NativeOutcome<'a>, NativeGateError> {
    let workspace = match Workspace::create(directory) {
        Ok(workspace) => workspace,
        Err(error) => {
            return combine(
                Err(error),
                port.finish().map_err(|error| {
                    NativeGateError::new("native supervisor cleanup", Failure::Process(error))
                }),
            )
        }
    };
    let operation = (|| {
        captured
            .write(&workspace.source, &workspace.state)
            .map_err(|error| {
                NativeGateError::new("native source clone", Failure::Source(error.to_string()))
            })?;
        let mut command = workspace.command(&plan)?;
        port.run(&mut command, plan.gate.budget())
            .map_err(|error| NativeGateError::new("native gate", Failure::Process(error)))
    })();
    let operation = combine(
        operation,
        port.finish().map_err(|error| {
            NativeGateError::new("native supervisor cleanup", Failure::Process(error))
        }),
    );
    let operation = combine(
        operation,
        captured.check(&workspace.source).map_err(|error| {
            NativeGateError::new(
                "captured native source check",
                Failure::Source(error.to_string()),
            )
        }),
    );
    combine(
        operation,
        fs::remove_dir_all(&workspace.root).map_err(|error| {
            NativeGateError::new("owned native state cleanup", Failure::Io(error))
        }),
    )?;
    Ok(match plan.gate {
        NativeGate::Fast => NativeOutcome::Fast(FastOutcome { captured }),
        NativeGate::Security => NativeOutcome::Security(SecurityOutcome { captured }),
    })
}

fn verify(
    captured: &CapturedReleaseSource,
    gate: NativeGate,
) -> Result<NativeOutcome<'_>, NativeGateError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            NativeGateError::new(
                "native execution clock",
                Failure::Io(io::Error::other(error)),
            )
        })?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-native-{}-{}-{stamp}",
        gate.entry(),
        std::process::id()
    ));
    let supervisor = CommandSupervisor::new()
        .map_err(|error| NativeGateError::new("native supervisor", Failure::Process(error)))?;
    verify_with(captured, NativePlan::new(gate), &directory, supervisor)
}

impl CapturedReleaseSource {
    /// Runs the maintained fast gate from the captured inputs.
    /// Releases supervised descendants and owned state before returning.
    pub fn verify_fast(&self) -> Result<FastOutcome<'_>, NativeGateError> {
        match verify(self, NativeGate::Fast)? {
            NativeOutcome::Fast(outcome) => Ok(outcome),
            NativeOutcome::Security(_) => Err(NativeGateError::new(
                "native fast outcome",
                Failure::Contract("security outcome cannot satisfy the fast gate"),
            )),
        }
    }

    /// Runs the maintained security gate from the captured inputs.
    /// Releases supervised descendants and owned state before returning.
    pub fn verify_security(&self) -> Result<SecurityOutcome<'_>, NativeGateError> {
        match verify(self, NativeGate::Security)? {
            NativeOutcome::Security(outcome) => Ok(outcome),
            NativeOutcome::Fast(_) => Err(NativeGateError::new(
                "native security outcome",
                Failure::Contract("fast outcome cannot satisfy the security gate"),
            )),
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
