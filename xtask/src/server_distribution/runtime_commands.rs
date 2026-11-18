use crate::process::{CommandSupervisor, ProcessError, RunningCommand, SupervisedCommand};
#[cfg(unix)]
use std::process::Command;
use std::{io, time::Duration};

pub(crate) struct RuntimeCommands<'a, F> {
    supervisor: &'a mut CommandSupervisor,
    configure: &'a mut F,
}

impl<'a, F> RuntimeCommands<'a, F> {
    pub(crate) fn new(supervisor: &'a mut CommandSupervisor, configure: &'a mut F) -> Self {
        Self {
            supervisor,
            configure,
        }
    }
}

#[cfg(unix)]
impl<F> RuntimeCommands<'_, F>
where
    F: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    pub(crate) fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        (self.configure)(command, budget)?;
        self.supervisor.run(command, budget)
    }

    pub(crate) fn run_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        (self.configure)(command, budget)?;
        self.supervisor.run_cleanup(command, budget)
    }

    pub(crate) fn start_service(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<RuntimeService<'_, F>, ProcessError> {
        (self.configure)(command, budget)?;
        let running = self.supervisor.start_service(command, budget)?;
        Ok(RuntimeService {
            running,
            configure: &mut *self.configure,
        })
    }
}

impl<F> RuntimeCommands<'_, F>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    pub(crate) fn run_owned(
        &mut self,
        mut command: SupervisedCommand,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        (self.configure)(&mut command, budget)?;
        self.supervisor.run_owned(command, budget)
    }

    pub(crate) fn start_service_owned(
        &mut self,
        mut command: SupervisedCommand,
        budget: Duration,
    ) -> Result<RuntimeService<'_, F>, ProcessError> {
        (self.configure)(&mut command, budget)?;
        let running = self.supervisor.start_service_owned(command, budget)?;
        Ok(RuntimeService {
            running,
            configure: &mut *self.configure,
        })
    }
}

pub(crate) struct RuntimeService<'a, F> {
    running: RunningCommand<'a>,
    configure: &'a mut F,
}

impl<'a, F> RuntimeService<'a, F> {
    pub(super) fn running(&mut self) -> &mut RunningCommand<'a> {
        &mut self.running
    }

    pub(crate) fn poll(&mut self) -> Result<(), ProcessError> {
        self.running.poll()
    }

    #[cfg(windows)]
    pub(crate) fn process_id(&mut self) -> Result<u32, ProcessError> {
        self.running.process_id()
    }

    pub(crate) fn stop(self) -> Result<(), ProcessError> {
        self.running.stop()
    }
}

#[cfg(unix)]
impl<F> RuntimeService<'_, F>
where
    F: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    pub(crate) fn run(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        (self.configure)(command, budget)?;
        self.running.run(command, budget)
    }

    pub(crate) fn stop_with(
        self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        match (self.configure)(command, budget) {
            Ok(()) => self.running.stop_with(command, budget),
            Err(operation) => match self.running.stop() {
                Ok(()) => Err(operation.into()),
                Err(cleanup) => Err(ProcessError::Cleanup {
                    operation: operation.to_string(),
                    cleanup: cleanup.to_string(),
                }),
            },
        }
    }
}

impl<F> RuntimeService<'_, F>
where
    F: FnMut(&mut SupervisedCommand, Duration) -> io::Result<()>,
{
    pub(crate) fn run_owned(
        &mut self,
        mut command: SupervisedCommand,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        (self.configure)(&mut command, budget)?;
        self.running.run_owned(command, budget)
    }
}
