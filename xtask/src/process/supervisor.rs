use super::{ProcessError, SupervisedCommand};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[cfg(unix)]
mod service;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub use service::RunningCommand;
#[cfg(unix)]
use unix as native;
#[cfg(windows)]
use windows as native;

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
