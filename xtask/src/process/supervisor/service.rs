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

    pub fn stop(mut self) -> Result<(), ProcessError> {
        let running = self.running.take().ok_or(ProcessError::InvalidOutput)?;
        self.supervisor
            .state
            .as_mut()
            .ok_or(ProcessError::InvalidOutput)?
            .stop_service(running)
    }
}
