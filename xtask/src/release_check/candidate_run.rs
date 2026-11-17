#[cfg(unix)]
use super::{CapturedReleaseSource, InputError};
use crate::process::ProcessError;
#[cfg(unix)]
use std::io;

#[cfg(unix)]
mod owned;
#[cfg(unix)]
pub(super) use owned::{checksum, copy_checked, Loaded, Paths, Workspace};

enum Failure {
    #[cfg(unix)]
    Io(String),
    #[cfg(unix)]
    Source(String),
    #[cfg(unix)]
    Inputs(InputError),
    Process(String),
    #[cfg(unix)]
    Consumer(String),
    #[cfg(unix)]
    Contract(&'static str),
}

pub(super) struct RunError {
    preceding: Option<String>,
    stage: &'static str,
    failure: Failure,
}

impl RunError {
    #[cfg(unix)]
    pub(super) fn io(stage: &'static str, error: io::Error) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Io(error.to_string()),
        }
    }
    #[cfg(unix)]
    pub(super) fn source(stage: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Source(error.to_string()),
        }
    }
    #[cfg(unix)]
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
    #[cfg(unix)]
    pub(super) fn consumer(stage: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            preceding: None,
            stage,
            failure: Failure::Consumer(error.to_string()),
        }
    }
    #[cfg(unix)]
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
            #[cfg(unix)]
            Failure::Io(cause) | Failure::Source(cause) | Failure::Consumer(cause) => {
                formatter.write_str(cause)
            }
            #[cfg(unix)]
            Failure::Inputs(cause) => cause.fmt(formatter),
            #[cfg(unix)]
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

#[cfg(unix)]
enum Preservation<'a> {
    Server,
    Oci { checksum: &'a str },
}

#[cfg(unix)]
pub(super) fn conclude(
    operation: Result<(), RunError>,
    loaded: &Loaded,
    captured: &CapturedReleaseSource,
    workspace: Workspace,
    finish: impl FnOnce() -> Result<(), ProcessError>,
) -> Result<(), RunError> {
    conclude_with_preservation(
        operation,
        loaded,
        captured,
        workspace,
        finish,
        Preservation::Server,
    )
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
        Preservation::Oci { checksum },
    )
}

#[cfg(unix)]
fn conclude_with_preservation(
    operation: Result<(), RunError>,
    loaded: &Loaded,
    captured: &CapturedReleaseSource,
    workspace: Workspace,
    finish: impl FnOnce() -> Result<(), ProcessError>,
    preservation: Preservation<'_>,
) -> Result<(), RunError> {
    let operation = combine(
        operation,
        finish().map_err(|error| RunError::process("candidate supervisor cleanup", error)),
    );
    let operation = combine(operation, loaded.check(captured, &workspace.source()));
    let operation = combine(operation, loaded.preserve(captured, workspace.artifacts()));
    let operation = match preservation {
        Preservation::Server => operation,
        Preservation::Oci { checksum } => combine(
            operation,
            copy_checked(
                &workspace.archive(),
                &workspace.artifacts().join("image.oci.tar"),
                checksum,
                512 * 1024 * 1024,
            )
            .map_err(|error| RunError::io("OCI archive preservation", error)),
        ),
    };
    combine(operation, workspace.close())
}

#[cfg(all(test, unix))]
mod finalization_tests;

#[cfg(unix)]
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
