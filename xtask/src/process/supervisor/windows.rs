use crate::process::{CommandStream, ProcessError, SupervisedCommand};
use rusterisk_windows_job_accounting::active_processes;
use std::{
    io,
    os::windows::io::AsHandle,
    path::{Path, PathBuf},
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use tokio::signal::windows::{ctrl_c, CtrlC};
use windows_spawn::{Child, CreationFlags, DropPolicy, Job, SpawnOptions};

use super::Interruption;

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLEANUP_BUDGET: Duration = Duration::from_secs(5);

pub const WINDOWS_CONSOLE_SIGNAL_COMMAND: &str = "signal-owned-console";

pub fn signal_owned_console(process_id: u32) -> io::Result<()> {
    if matches!(process_id, 0 | u32::MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid owned console process",
        ));
    }
    win32console::console::WinConsole::attach_console(process_id)
        .map_err(|error| io::Error::other(format!("attach to owned console: {error}")))?;
    winconsole::console::set_ctrl_handler(None, true)
        .map_err(|error| io::Error::other(format!("configure console signal helper: {error}")))?;
    winconsole::console::generate_ctrl_event(false, None)
        .map_err(|error| io::Error::other(format!("signal owned console: {error}")))
}

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Command,
    Cleanup,
}

pub(super) struct Supervisor {
    interrupt: CtrlC,
    interruption: Interruption,
    console_helper: Option<PathBuf>,
}

pub(super) struct Service {
    tree: CommandTree,
    pub(super) deadline: Instant,
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
    fn start(command: SupervisedCommand, flags: CreationFlags) -> Result<Self, ProcessError> {
        let job = Job::create()?;
        job.set_kill_on_close(true)?;
        let child = command.into_windows().spawn_with(
            SpawnOptions::new()
                .job(&job)
                .creation_flags(flags)
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

pub(super) fn abort_service(service: Service) -> Result<(), ProcessError> {
    service.tree.close()
}

impl Supervisor {
    pub(super) fn new() -> Result<Self, ProcessError> {
        let interrupt = ctrl_c()?;
        winconsole::console::set_ctrl_handler(None, false)
            .map_err(|error| io::Error::other(format!("enable console interruptions: {error}")))?;
        Ok(Self {
            interrupt,
            interruption: Interruption::Quiet,
            console_helper: None,
        })
    }

    pub(super) fn with_console_helper(path: &Path) -> Result<Self, ProcessError> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "console signal helper requires an absolute path",
            )
            .into());
        }
        let mut supervisor = Self::new()?;
        supervisor.console_helper = Some(path.to_owned());
        Ok(supervisor)
    }

    pub(super) fn start_service(
        &mut self,
        command: SupervisedCommand,
        deadline: Instant,
    ) -> Result<Service, ProcessError> {
        self.poll(Purpose::Command)?;
        let tree = CommandTree::start(command, CreationFlags::NEW_CONSOLE)?;
        Ok(Service { tree, deadline })
    }

    pub(super) fn poll_service(&mut self, service: &mut Service) -> Result<(), ProcessError> {
        self.poll(Purpose::Command)?;
        if Instant::now() >= service.deadline {
            return Err(ProcessError::Timeout);
        }
        if service.tree.child.try_wait()?.is_some() {
            return Err(ProcessError::InvalidOutput);
        }
        Ok(())
    }

    fn interrupt_service(
        &mut self,
        process_id: u32,
        deadline: Instant,
    ) -> Result<(), ProcessError> {
        let executable;
        let helper = match self.console_helper.as_deref() {
            Some(path) => path,
            None => {
                executable = std::env::current_exe()?;
                &executable
            }
        };
        let mut command = SupervisedCommand::new(helper).map_err(io::Error::other)?;
        command
            .arg(WINDOWS_CONSOLE_SIGNAL_COMMAND)
            .and_then(|command| command.arg(process_id.to_string()))
            .map_err(io::Error::other)?;
        command.stderr(CommandStream::Inherit);
        let mut helper = CommandTree::start(command, CreationFlags::DETACHED_PROCESS)?;
        let operation = self.monitor(&mut helper, deadline, Purpose::Cleanup);
        combine(operation, helper.close())
    }

    pub(super) fn stop_service(&mut self, mut service: Service) -> Result<(), ProcessError> {
        let deadline = Instant::now() + CLEANUP_BUDGET;
        let operation = (|| {
            if service.tree.child.try_wait()?.is_none() {
                self.interrupt_service(service.tree.child.id(), deadline)?;
            }
            self.monitor(&mut service.tree, deadline, Purpose::Cleanup)
        })();
        combine(operation, service.tree.close())
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
        let mut tree = CommandTree::start(command, CreationFlags::default())?;
        let operation = self.monitor(&mut tree, deadline, purpose);
        combine(operation, tree.close())
    }

    pub(super) fn finish(mut self) -> Result<(), ProcessError> {
        self.observe()?;
        self.interruption.finish()
    }
}
