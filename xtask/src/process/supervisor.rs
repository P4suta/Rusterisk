use super::ProcessError;
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[cfg(unix)]
mod unix;

pub struct CommandSupervisor {
    #[cfg(unix)]
    state: Option<unix::Supervisor>,
}

impl CommandSupervisor {
    pub fn new() -> Result<Self, ProcessError> {
        #[cfg(unix)]
        {
            Ok(Self {
                state: Some(unix::Supervisor::new()?),
            })
        }
        #[cfg(not(unix))]
        {
            Err(ProcessError::UnsupportedSupervision)
        }
    }

    pub fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        let deadline = Instant::now()
            .checked_add(budget)
            .filter(|_| !budget.is_zero())
            .ok_or(ProcessError::InvalidBudget)?;
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

    pub fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        let deadline = Instant::now()
            .checked_add(budget)
            .filter(|_| !budget.is_zero())
            .ok_or(ProcessError::InvalidBudget)?;
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
        #[cfg(unix)]
        {
            let mut owned = self;
            owned
                .state
                .take()
                .ok_or(ProcessError::InvalidOutput)?
                .finish()
        }
        #[cfg(not(unix))]
        {
            Err(ProcessError::UnsupportedSupervision)
        }
    }
}

impl Drop for CommandSupervisor {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(state) = self.state.take() {
            if let Err(error) = state.finish() {
                eprintln!("process supervisor cleanup failed: {error}");
            }
        }
    }
}
