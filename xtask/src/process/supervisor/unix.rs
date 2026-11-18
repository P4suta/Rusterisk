use crate::process::ProcessError;
use rustix::process::{
    kill_process, kill_process_group, waitid, Pid, Signal, WaitId, WaitIdOptions,
};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    low_level::{pipe, unregister},
    SigId,
};
use std::{
    io::{self, Read},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub(super) struct Supervisor {
    reader: UnixStream,
    registrations: [SigId; 2],
    interruption: Interruption,
}

enum Interruption {
    Quiet,
    Pending,
    Reported,
}

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

pub(super) struct Service {
    group: Group,
    pid: Pid,
    pub(super) deadline: Instant,
}

fn registration_failure(operation: io::Error, removed: bool) -> ProcessError {
    if removed {
        operation.into()
    } else {
        ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: "signal registration was not found".to_owned(),
        }
    }
}

fn peek(pid: Pid) -> io::Result<bool> {
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
    )
    .map(|status| status.is_some())
    .map_err(io::Error::from)
}

fn reap(child: &mut Child, pid: Pid, deadline: Instant) -> Result<ExitStatus, ProcessError> {
    loop {
        match peek(pid) {
            Ok(true) => return child.wait().map_err(ProcessError::from),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(ProcessError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Children {
    anchor: Child,
    command: Option<Child>,
}

struct Group {
    children: Option<Children>,
    pid: Pid,
}

fn cleanup_error(errors: &mut String, error: impl std::fmt::Display) {
    if !errors.is_empty() {
        errors.push_str("; ");
    }
    errors.push_str(&error.to_string());
}

fn observe_anchor(pid: Pid, deadline: Instant) -> io::Result<bool> {
    loop {
        match peek(pid) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "anchor ownership observation deadline exceeded",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn signal_group(pid: Pid) -> io::Result<()> {
    match kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
    }
}

fn stop_with(
    children: &mut Children,
    pid: Pid,
    signal: impl FnOnce(Pid) -> io::Result<()>,
) -> Result<Option<ExitStatus>, ProcessError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut errors = String::new();
    let anchor_exited = observe_anchor(pid, deadline);
    let killed = match anchor_exited {
        Ok(true) if children.command.is_none() => Ok(()),
        Ok(_) => signal(pid),
        Err(error) => Err(error),
    };
    if let Err(error) = killed {
        cleanup_error(&mut errors, error);
    }
    drop(children.anchor.stdin.take());
    let command = if let Some(child) = &mut children.command {
        match reap(child, Pid::from_child(child), deadline) {
            Ok(status) => Some(status),
            Err(error) => {
                cleanup_error(&mut errors, error);
                None
            }
        }
    } else {
        None
    };
    if let Err(error) = reap(&mut children.anchor, pid, deadline) {
        cleanup_error(&mut errors, error);
    }
    if errors.is_empty() {
        Ok(command)
    } else {
        Err(ProcessError::Cleanup {
            operation: "owned process group termination".to_owned(),
            cleanup: errors,
        })
    }
}

fn stop(children: &mut Children, pid: Pid) -> Result<Option<ExitStatus>, ProcessError> {
    stop_with(children, pid, signal_group)
}

impl Group {
    fn new() -> Result<Self, ProcessError> {
        Self::with_anchor(&mut Command::new("/bin/cat"))
    }

    fn with_anchor(command: &mut Command) -> Result<Self, ProcessError> {
        let anchor = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        let pid = Pid::from_child(&anchor);
        Ok(Self {
            children: Some(Children {
                anchor,
                command: None,
            }),
            pid,
        })
    }

    fn start(&mut self, command: &mut Command) -> Result<Pid, ProcessError> {
        if peek(self.pid)? {
            return Err(io::Error::other("process group anchor exited early").into());
        }
        let children = self.children.as_mut().ok_or(ProcessError::InvalidOutput)?;
        let child = command
            .stdin(Stdio::null())
            .process_group(self.pid.as_raw_pid())
            .spawn()?;
        let pid = Pid::from_child(&child);
        children.command = Some(child);
        Ok(pid)
    }

    fn finish(self) -> Result<Option<ExitStatus>, ProcessError> {
        self.finish_with(signal_group)
    }

    fn finish_with(
        mut self,
        signal: impl FnOnce(Pid) -> io::Result<()>,
    ) -> Result<Option<ExitStatus>, ProcessError> {
        let mut children = self.children.take().ok_or(ProcessError::InvalidOutput)?;
        stop_with(&mut children, self.pid, signal)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(mut children) = self.children.take() {
            if let Err(error) = stop(&mut children, self.pid) {
                eprintln!("process group cleanup failed: {error}");
            }
        }
    }
}

fn completed_service(
    service: Service,
    operation: Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    let status = service.group.finish();
    match (operation, status) {
        (Ok(()), Ok(Some(status))) if status.success() => Ok(()),
        (Ok(()), Ok(Some(status))) => Err(ProcessError::Status(status)),
        (Ok(()), Ok(None)) => Err(ProcessError::InvalidOutput),
        (Err(error), Ok(_)) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

pub(super) fn abort_service(service: Service) -> Result<(), ProcessError> {
    service
        .group
        .finish()?
        .ok_or(ProcessError::InvalidOutput)
        .map(|_| ())
}

impl Supervisor {
    pub(super) fn start_service(
        &mut self,
        command: &mut Command,
        deadline: Instant,
    ) -> Result<Service, ProcessError> {
        if self.pending()? {
            self.interruption.observe();
        }
        self.interruption.report()?;
        let mut group = Group::new()?;
        let pid = group.start(command)?;
        Ok(Service {
            group,
            pid,
            deadline,
        })
    }

    pub(super) fn poll_service(&mut self, service: &Service) -> Result<(), ProcessError> {
        if self.pending()? {
            self.interruption.observe();
        }
        self.interruption.report()?;
        if Instant::now() >= service.deadline {
            return Err(ProcessError::Timeout);
        }
        if peek(service.pid)? || peek(service.group.pid)? {
            return Err(ProcessError::InvalidOutput);
        }
        Ok(())
    }

    pub(super) fn stop_service(&mut self, service: Service) -> Result<(), ProcessError> {
        let operation = (|| {
            if !peek(service.pid)? {
                match kill_process(service.pid, Signal::TERM) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(error) => return Err(io::Error::from(error).into()),
                }
            }
            self.monitor(
                service.pid,
                service.group.pid,
                Instant::now() + Duration::from_secs(5),
                true,
            )
        })();
        completed_service(service, operation)
    }

    pub(super) fn wait_service(&mut self, service: Service) -> Result<(), ProcessError> {
        let deadline = (Instant::now() + Duration::from_secs(5)).min(service.deadline);
        let operation = self.monitor(service.pid, service.group.pid, deadline, false);
        completed_service(service, operation)
    }
    pub(super) fn new() -> Result<Self, ProcessError> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        let second = writer.try_clone()?;
        let interrupt = pipe::register(SIGINT, writer)?;
        let terminate = match pipe::register(SIGTERM, second) {
            Ok(id) => id,
            Err(error) => return Err(registration_failure(error, unregister(interrupt))),
        };
        Ok(Self {
            reader,
            registrations: [interrupt, terminate],
            interruption: Interruption::Quiet,
        })
    }

    fn pending(&self) -> io::Result<bool> {
        let mut reader = &self.reader;
        let mut bytes = [0; 128];
        match reader.read(&mut bytes) {
            Ok(0) => Err(io::Error::other("process signal pipe closed")),
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn monitor(
        &mut self,
        pid: Pid,
        anchor: Pid,
        deadline: Instant,
        cleanup: bool,
    ) -> Result<(), ProcessError> {
        loop {
            if self.pending()? {
                self.interruption.observe();
            }
            if !cleanup {
                self.interruption.report()?;
            }
            match peek(anchor) {
                Ok(true) => {
                    return Err(io::Error::other("process group anchor exited early").into())
                }
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
            match peek(pid) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProcessError::Timeout);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub(super) fn run(
        &mut self,
        command: &mut Command,
        deadline: Instant,
        cleanup: bool,
    ) -> Result<(), ProcessError> {
        if self.pending()? {
            self.interruption.observe();
        }
        if !cleanup {
            self.interruption.report()?;
        }
        let mut group = Group::new()?;
        let operation = (|| {
            let pid = group.start(command)?;
            self.monitor(pid, group.pid, deadline, cleanup)
        })();
        let status = group.finish();
        match (operation, status) {
            (Ok(()), Ok(Some(status))) if status.success() => Ok(()),
            (Ok(()), Ok(Some(status))) => Err(ProcessError::Status(status)),
            (Ok(()), Ok(None)) => Err(ProcessError::InvalidOutput),
            (Err(error), Ok(_)) | (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
                operation: operation.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }

    pub(super) fn finish(mut self) -> Result<(), ProcessError> {
        let operation = self
            .pending()
            .map_err(ProcessError::from)
            .and_then(|pending| {
                if pending {
                    self.interruption.observe();
                }
                self.interruption.finish()
            });
        let removed = self.registrations.map(unregister);
        let cleanup = if removed == [true, true] {
            Ok(())
        } else {
            Err(io::Error::other("process signal registration cleanup failed").into())
        };
        match (operation, cleanup) {
            (operation, Ok(())) => operation,
            (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
                operation: operation.to_string(),
                cleanup: cleanup.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
