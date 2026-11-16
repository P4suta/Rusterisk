use crate::{
    core_assurance::{AssuranceError, Phase},
    process::ProcessError,
};
mod capture;
mod runner;
pub use crate::core_assurance::Execution;
pub use capture::CapturedCoreMiri;
pub use runner::{execute, plan, CoreMiriOutcome};

pub enum CoreMiriError {
    Io(std::io::Error),
    Assurance(AssuranceError),
    Process(ProcessError),
    Phase {
        phase: Phase,
        source: ProcessError,
    },
    Changed,
    Plan,
    FollowUp {
        operation: String,
        context: &'static str,
        follow_up: String,
    },
}
impl std::fmt::Debug for CoreMiriError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}
impl std::fmt::Display for CoreMiriError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Assurance(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Phase { phase, source } => {
                write!(formatter, "Miri phase {}: {source}", phase.label())
            }
            Self::Changed => formatter.write_str("captured or normalized Miri inputs changed"),
            Self::Plan => formatter.write_str("invalid bounded Miri input or execution plan"),
            Self::FollowUp {
                operation,
                context,
                follow_up,
            } => write!(formatter, "{operation}; {context}: {follow_up}"),
        }
    }
}
impl std::error::Error for CoreMiriError {}
impl From<std::io::Error> for CoreMiriError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<AssuranceError> for CoreMiriError {
    fn from(error: AssuranceError) -> Self {
        Self::Assurance(error)
    }
}
