use super::{ProcessError, SupervisedCommand};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[cfg(any(unix, windows))]
mod service;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(any(unix, windows))]
pub use service::RunningCommand;
#[cfg(unix)]
use unix as native;
#[cfg(windows)]
use windows as native;
#[cfg(windows)]
pub use windows::{signal_owned_console, WINDOWS_CONSOLE_SIGNAL_COMMAND};

fn deadline(budget: Duration) -> Result<Instant, ProcessError> {
    Instant::now()
        .checked_add(budget)
        .filter(|_| !budget.is_zero())
        .ok_or(ProcessError::InvalidBudget)
}

#[cfg(any(unix, windows))]
enum Interruption {
    Quiet,
    Pending,
    Reported,
}

#[cfg(any(unix, windows))]
impl Interruption {
    fn observe(&mut self) {
        match self {
            Self::Quiet => *self = Self::Pending,
            Self::Pending | Self::Reported => {}
        }
    }

    fn report(&mut self) -> Result<(), ProcessError> {
        match self {
            Self::Quiet => Ok(()),
            Self::Pending | Self::Reported => {
                *self = Self::Reported;
                Err(ProcessError::Interrupted)
            }
        }
    }

    fn finish(&self) -> Result<(), ProcessError> {
        match self {
            Self::Quiet | Self::Reported => Ok(()),
            Self::Pending => Err(ProcessError::Interrupted),
        }
    }
}

pub struct CommandSupervisor {
    #[cfg(any(unix, windows))]
    state: Option<native::Supervisor>,
}

impl CommandSupervisor {
    #[cfg(unix)]
    pub fn start_service(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<RunningCommand<'_>, ProcessError> {
        let deadline = deadline(budget)?;
        let running = self
            .state
            .as_mut()
            .ok_or(ProcessError::InvalidOutput)?
            .start_service(command, deadline)?;
        Ok(RunningCommand::new(self, running))
    }
    pub fn new() -> Result<Self, ProcessError> {
        #[cfg(any(unix, windows))]
        {
            Ok(Self {
                state: Some(native::Supervisor::new()?),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(ProcessError::UnsupportedSupervision)
        }
    }

    #[cfg(windows)]
    pub fn with_console_helper(path: &std::path::Path) -> Result<Self, ProcessError> {
        Ok(Self {
            state: Some(windows::Supervisor::with_console_helper(path)?),
        })
    }

    #[cfg(any(unix, windows))]
    pub fn start_service_owned(
        &mut self,
        command: SupervisedCommand,
        budget: Duration,
    ) -> Result<RunningCommand<'_>, ProcessError> {
        let deadline = deadline(budget)?;
        let state = self.state.as_mut().ok_or(ProcessError::InvalidOutput)?;
        #[cfg(unix)]
        let running = state.start_service(&mut command.into_std(), deadline)?;
        #[cfg(windows)]
        let running = state.start_service(command, deadline)?;
        Ok(RunningCommand::new(self, running))
    }

    pub fn run_owned(
        &mut self,
        command: SupervisedCommand,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        #[cfg(windows)]
        {
            self.state.as_mut().ok_or(ProcessError::InvalidOutput)?.run(
                command,
                deadline(budget)?,
                windows::Purpose::Command,
            )
        }
        #[cfg(not(windows))]
        {
            self.run(&mut command.into_std(), budget)
        }
    }

    pub fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        let deadline = deadline(budget)?;
        #[cfg(unix)]
        {
            self.state
                .as_mut()
                .ok_or(ProcessError::InvalidOutput)?
                .run(command, deadline, false)
        }
        #[cfg(not(unix))]
        {
            let _ = (command, deadline);
            Err(ProcessError::UnsupportedSupervision)
        }
    }

    pub fn run_cleanup_owned(
        &mut self,
        command: SupervisedCommand,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        #[cfg(windows)]
        {
            self.state.as_mut().ok_or(ProcessError::InvalidOutput)?.run(
                command,
                deadline(budget)?,
                windows::Purpose::Cleanup,
            )
        }
        #[cfg(not(windows))]
        {
            self.run_cleanup(&mut command.into_std(), budget)
        }
    }

    pub fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        let deadline = deadline(budget)?;
        #[cfg(unix)]
        {
            self.state
                .as_mut()
                .ok_or(ProcessError::InvalidOutput)?
                .run(command, deadline, true)
        }
        #[cfg(not(unix))]
        {
            let _ = (command, deadline);
            Err(ProcessError::UnsupportedSupervision)
        }
    }

    pub fn finish(self) -> Result<(), ProcessError> {
        #[cfg(any(unix, windows))]
        {
            let mut owned = self;
            owned
                .state
                .take()
                .ok_or(ProcessError::InvalidOutput)?
                .finish()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(ProcessError::UnsupportedSupervision)
        }
    }
}

impl Drop for CommandSupervisor {
    fn drop(&mut self) {
        #[cfg(any(unix, windows))]
        if let Some(state) = self.state.take() {
            if let Err(error) = state.finish() {
                eprintln!("process supervisor cleanup failed: {error}");
            }
        }
    }
}
