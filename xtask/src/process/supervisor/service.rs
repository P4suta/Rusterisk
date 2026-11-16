use super::{unix, CommandSupervisor};
use crate::process::ProcessError;
use std::{
    process::Command,
    time::{Duration, Instant},
};

pub struct RunningCommand<'a> {
    supervisor: &'a mut CommandSupervisor,
    running: Option<unix::Service>,
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
    pub(super) fn new(supervisor: &'a mut CommandSupervisor, running: unix::Service) -> Self {
        Self {
            supervisor,
            running: Some(running),
        }
    }

    pub fn poll(&mut self) -> Result<(), ProcessError> {
        let service = self.running.as_ref().ok_or(ProcessError::InvalidOutput)?;
        self.supervisor
            .state
            .as_mut()
            .ok_or(ProcessError::InvalidOutput)?
            .poll_service(service)
    }

    pub fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
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
        let result = self.supervisor.run(command, budget.min(remaining));
        let service = self.poll();
        match (result, service) {
            (result, Ok(())) => result,
            (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
                operation: operation.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }

    /// Runs a bounded stop request and waits for the service's actual zero exit.
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
                    unix::abort_service(running),
                ),
            }
        } else {
            unix::abort_service(running)
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
