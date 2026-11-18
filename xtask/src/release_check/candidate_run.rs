#[cfg(any(unix, windows))]
use super::{CapturedReleaseSource, InputError};
use crate::process::ProcessError;
#[cfg(any(unix, windows))]
use std::io;

#[cfg(any(unix, windows))]
mod owned;
#[cfg(unix)]
pub(super) use owned::{checksum, copy_checked};
#[cfg(any(unix, windows))]
pub(super) use owned::{Loaded, Paths, Workspace};

enum Failure {
    #[cfg(any(unix, windows))]
    Io(String),
    #[cfg(any(unix, windows))]
    Source(String),
    #[cfg(any(unix, windows))]
    Inputs(InputError),
    Process(String),
    #[cfg(any(unix, windows))]
    Consumer(String),
    #[cfg(any(unix, windows))]
    Contract(&'static str),
}

pub(super) struct RunError {
    preceding: Option<String>,
    stage: &'static str,
    failure: Failure,
}

impl RunError {
    #[cfg(any(unix, windows))]
    pub(super) fn io(stage: &'static str, error: io::Error) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Io(error.to_string()),
        }
    }
    #[cfg(any(unix, windows))]
    pub(super) fn source(stage: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Source(error.to_string()),
        }
    }
    #[cfg(any(unix, windows))]
    pub(super) fn inputs(stage: &'static str, error: InputError) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Inputs(error),
        }
    }
    pub(super) fn process(stage: &'static str, error: ProcessError) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Process(error.to_string()),
        }
    }
    #[cfg(any(unix, windows))]
    pub(super) fn consumer(stage: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Consumer(error.to_string()),
        }
    }
    #[cfg(any(unix, windows))]
    pub(super) fn contract(stage: &'static str, reason: &'static str) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Contract(reason),
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(preceding) = &self.preceding {
            write!(formatter, "{preceding}; ")?;
        }
        write!(formatter, "{}: ", self.stage)?;
        match &self.failure {
            Failure::Process(cause) => formatter.write_str(cause),
            #[cfg(any(unix, windows))]
            Failure::Io(cause) | Failure::Source(cause) | Failure::Consumer(cause) => {
                formatter.write_str(cause)
            }
            #[cfg(any(unix, windows))]
            Failure::Inputs(cause) => cause.fmt(formatter),
            #[cfg(any(unix, windows))]
            Failure::Contract(reason) => formatter.write_str(reason),
        }
    }
}

impl std::fmt::Debug for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}
impl std::error::Error for RunError {}

#[cfg(any(unix, windows))]
pub(super) fn conclude(
    operation: Result<(), RunError>,
    loaded: &Loaded,
    captured: &CapturedReleaseSource,
    workspace: Workspace,
    finish: impl FnOnce() -> Result<(), ProcessError>,
) -> Result<(), RunError> {
    conclude_with_preservation(operation, loaded, captured, workspace, finish, |_| Ok(()))
}

#[cfg(unix)]
pub(super) fn conclude_oci(
    operation: Result<(), RunError>,
    loaded: &Loaded,
    captured: &CapturedReleaseSource,
    workspace: Workspace,
    checksum: &str,
    finish: impl FnOnce() -> Result<(), ProcessError>,
) -> Result<(), RunError> {
    conclude_with_preservation(
        operation,
        loaded,
        captured,
        workspace,
        finish,
        |workspace| {
            copy_checked(
                &workspace.archive(),
                &workspace.artifacts().join("image.oci.tar"),
                checksum,
                512 * 1024 * 1024,
            )
            .map_err(|error| RunError::io("OCI archive preservation", error))
        },
    )
}

#[cfg(any(unix, windows))]
fn conclude_with_preservation(
    operation: Result<(), RunError>,
    loaded: &Loaded,
    captured: &CapturedReleaseSource,
    workspace: Workspace,
    finish: impl FnOnce() -> Result<(), ProcessError>,
    preserve: impl FnOnce(&Workspace) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let operation = combine(
        operation,
        finish().map_err(|error| RunError::process("candidate supervisor cleanup", error)),
    );
    let operation = combine(operation, loaded.check(captured, &workspace.source()));
    let operation = combine(operation, loaded.preserve(captured, workspace.artifacts()));
    let operation = combine(operation, preserve(&workspace));
    combine(operation, workspace.close())
}

#[cfg(all(test, unix))]
mod finalization_tests;
#[cfg(all(test, windows))]
mod windows_workspace_tests;

#[cfg(any(unix, windows))]
pub(super) fn combine<T>(
    operation: Result<T, RunError>,
    follow_up: Result<(), RunError>,
) -> Result<T, RunError> {
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
