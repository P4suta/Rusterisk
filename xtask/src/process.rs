use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

mod request;
pub use request::{CommandField, CommandRequestError, CommandStream, SupervisedCommand};

#[derive(Debug)]
pub enum ProcessError {
    Io(std::io::Error),
    Status(ExitStatus),
    Timeout,
    InvalidBudget,
    InvalidOutput,
    Interrupted,
    UnsupportedSupervision,
    Cleanup { operation: String, cleanup: String },
}

impl From<std::io::Error> for ProcessError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "process failed: {error}"),
            Self::Status(status) => write!(formatter, "process failed: {status}"),
            Self::Timeout => formatter.write_str("process deadline exceeded"),
            Self::InvalidBudget => formatter.write_str("process budget must be positive"),
            Self::InvalidOutput => formatter.write_str("invalid bounded process output"),
            Self::Interrupted => formatter.write_str("process interrupted"),
            Self::UnsupportedSupervision => {
                formatter.write_str("process tree supervision is unavailable on this platform")
            }
            Self::Cleanup { operation, cleanup } => {
                write!(formatter, "{operation}; process cleanup failed: {cleanup}")
            }
        }
    }
}

impl std::error::Error for ProcessError {}

fn stop(child: &mut Child) -> Result<(), ProcessError> {
    if let Err(error) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(ProcessError::Io(error));
        }
    }
    child.wait()?;
    Ok(())
}

/// Reaps one child without process group or signal supervision.
pub fn run(command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
    let deadline = Instant::now()
        .checked_add(budget)
        .filter(|_| !budget.is_zero())
        .ok_or(ProcessError::InvalidBudget)?;
    let mut child = command.stdin(Stdio::null()).spawn()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(ProcessError::Status(status))
                }
            }
            Ok(None) => {}
            Err(error) => {
                stop(&mut child)?;
                return Err(ProcessError::Io(error));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop(&mut child)?;
            return Err(ProcessError::Timeout);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

mod supervisor;
pub use supervisor::CommandSupervisor;
#[cfg(unix)]
pub use supervisor::RunningCommand;
