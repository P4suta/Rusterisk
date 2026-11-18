use crate::process::{ProcessError, SupervisedCommand};
use rusterisk_windows_job_accounting::active_processes;
use std::{
    io,
    os::windows::io::AsHandle,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use tokio::signal::windows::{ctrl_c, CtrlC};
use windows_spawn::{Child, DropPolicy, Job, SpawnOptions};

use super::Interruption;

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLEANUP_BUDGET: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Command,
    Cleanup,
}

pub(super) struct Supervisor {
    interrupt: CtrlC,
    interruption: Interruption,
}

struct CommandTree {
    job: Job,
    child: Child,
}

fn combine<T>(
    operation: Result<T, ProcessError>,
    cleanup: Result<(), ProcessError>,
) -> Result<T, ProcessError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

fn pause(deadline: Instant) -> Result<(), ProcessError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ProcessError::Timeout);
    }
    std::thread::sleep(remaining.min(POLL_INTERVAL));
    Ok(())
}

fn wait_for<T>(
    deadline: Instant,
    mut observe: impl FnMut() -> io::Result<Option<T>>,
) -> Result<T, ProcessError> {
    loop {
        if let Some(value) = observe()? {
            return Ok(value);
        }
        pause(deadline)?;
    }
}

impl CommandTree {
    fn start(command: SupervisedCommand) -> Result<Self, ProcessError> {
        let job = Job::create()?;
        job.set_kill_on_close(true)?;
        let child = command.into_windows().spawn_with(
            SpawnOptions::new()
                .job(&job)
                .drop_policy(DropPolicy::Detach),
        )?;
        Ok(Self { job, child })
    }

    fn close(self) -> Result<(), ProcessError> {
        let Self { job, mut child } = self;
        let deadline = Instant::now() + CLEANUP_BUDGET;
        let termination = job.terminate(1).map_err(ProcessError::from);
        let leader = wait_for(deadline, || child.try_wait()).map(|_| ());
        drop(child);
        let empty = wait_for(deadline, || {
            active_processes(job.as_handle()).map(|count| (count == 0).then_some(()))
        });
        combine(combine(termination, leader), empty)
    }
}

impl Supervisor {
    pub(super) fn new() -> Result<Self, ProcessError> {
        Ok(Self {
            interrupt: ctrl_c()?,
            interruption: Interruption::Quiet,
        })
    }

    fn observe(&mut self) -> Result<(), ProcessError> {
        match self
            .interrupt
            .poll_recv(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(Some(())) => self.interruption.observe(),
            Poll::Pending => {}
            Poll::Ready(None) => {
                return Err(io::Error::other("process signal receiver closed").into())
            }
        }
        Ok(())
    }

    fn poll(&mut self, purpose: Purpose) -> Result<(), ProcessError> {
        self.observe()?;
        match purpose {
            Purpose::Command => self.interruption.report(),
            Purpose::Cleanup => Ok(()),
        }
    }

    fn monitor(
        &mut self,
        tree: &mut CommandTree,
        deadline: Instant,
        purpose: Purpose,
    ) -> Result<(), ProcessError> {
        loop {
            self.poll(purpose)?;
            if let Some(status) = tree.child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(ProcessError::Status(status))
                };
            }
            pause(deadline)?;
        }
    }

    pub(super) fn run(
        &mut self,
        command: SupervisedCommand,
        deadline: Instant,
        purpose: Purpose,
    ) -> Result<(), ProcessError> {
        self.poll(purpose)?;
        let mut tree = CommandTree::start(command)?;
        let operation = self.monitor(&mut tree, deadline, purpose);
        combine(operation, tree.close())
    }

    pub(super) fn finish(mut self) -> Result<(), ProcessError> {
        self.observe()?;
        self.interruption.finish()
    }
}
