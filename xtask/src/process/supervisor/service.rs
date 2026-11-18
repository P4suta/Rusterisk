use super::{native, CommandSupervisor};
use crate::process::{ProcessError, SupervisedCommand};
#[cfg(unix)]
use std::process::Command;
use std::time::{Duration, Instant};

pub struct RunningCommand<'a> {
    supervisor: &'a mut CommandSupervisor,
    running: Option<native::Service>,
}

fn stopped(
    operation: Result<(), ProcessError>,
    cleanup: Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

impl<'a> RunningCommand<'a> {
    pub(super) fn new(supervisor: &'a mut CommandSupervisor, running: native::Service) -> Self {
        Self {
            supervisor,
            running: Some(running),
        }
    }

    pub fn poll(&mut self) -> Result<(), ProcessError> {
        let service = self.running.as_mut().ok_or(ProcessError::InvalidOutput)?;
        self.supervisor
            .state
            .as_mut()
            .ok_or(ProcessError::InvalidOutput)?
            .poll_service(service)
    }

    #[cfg(windows)]
    pub(crate) fn process_id(&mut self) -> Result<u32, ProcessError> {
        self.poll()?;
        self.running
            .as_ref()
            .map(native::Service::process_id)
            .ok_or(ProcessError::InvalidOutput)
    }

    fn run_checked(
        &mut self,
        budget: Duration,
        run: impl FnOnce(&mut CommandSupervisor, Duration) -> Result<(), ProcessError>,
    ) -> Result<(), ProcessError> {
        self.poll()?;
        let remaining = self
            .running
            .as_ref()
            .ok_or(ProcessError::InvalidOutput)?
            .deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProcessError::Timeout);
        }
        let result = run(self.supervisor, budget.min(remaining));
        stopped(result, self.poll())
    }

    #[cfg(unix)]
    pub fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        self.run_checked(budget, |supervisor, budget| supervisor.run(command, budget))
    }

    pub fn run_owned(
        &mut self,
        command: SupervisedCommand,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        self.run_checked(budget, |supervisor, budget| {
            supervisor.run_owned(command, budget)
        })
    }

    /// Runs a bounded stop request and waits for the service's actual zero exit.
    #[cfg(unix)]
    pub fn stop_with(
        mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        let operation = if budget.is_zero() {
            Err(ProcessError::InvalidBudget)
        } else {
            self.poll().and_then(|()| {
                let remaining = self
                    .running
                    .as_ref()
                    .ok_or(ProcessError::InvalidOutput)?
                    .deadline
                    .saturating_duration_since(Instant::now());
                self.supervisor.run(command, budget.min(remaining))
            })
        };
        let running = self.running.take().ok_or(ProcessError::InvalidOutput)?;
        let cleanup = if operation.is_ok() {
            match self.supervisor.state.as_mut() {
                Some(state) => state.wait_service(running),
                None => stopped(
                    Err(ProcessError::InvalidOutput),
                    native::abort_service(running),
                ),
            }
        } else {
            native::abort_service(running)
        };
        stopped(operation, cleanup)
    }

    pub fn stop(mut self) -> Result<(), ProcessError> {
        let running = self.running.take().ok_or(ProcessError::InvalidOutput)?;
        self.supervisor
            .state
            .as_mut()
            .ok_or(ProcessError::InvalidOutput)?
            .stop_service(running)
    }
}

#[cfg(windows)]
impl Drop for RunningCommand<'_> {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            if let Err(error) = native::abort_service(running) {
                eprintln!("process service cleanup failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_stop_keeps_the_primary_and_cleanup_failures() -> Result<(), std::io::Error> {
        for operation in [ProcessError::Timeout, ProcessError::Interrupted] {
            let expected = operation.to_string();
            let result = stopped(
                Err(operation),
                Err(std::io::Error::other("cleanup fixture").into()),
            );
            let error = result
                .err()
                .ok_or_else(|| std::io::Error::other("failed stop passed"))?;
            assert!(matches!(&error, ProcessError::Cleanup { .. }));
            let message = error.to_string();
            assert!(message.contains(&expected));
            assert!(message.contains("cleanup fixture"));
        }
        Ok(())
    }
}
