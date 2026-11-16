use crate::{
    process::{CommandSupervisor, ProcessError},
    server_distribution::native::{combine, read},
};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub(super) struct Capture {
    pub command: Command,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Capture {
    pub fn read(&self) -> io::Result<Observed> {
        Ok(Observed {
            stdout: read(&self.stdout, 1024 * 1024)?,
            stderr: read(&self.stderr, 1024 * 1024)?,
        })
    }
}

pub(super) struct Observed {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Observed {
    pub fn exact(&self, expected: &[u8]) -> io::Result<()> {
        if self.stdout != expected || !self.stderr.is_empty() {
            return Err(io::Error::other("OCI command output changed its contract"));
        }
        Ok(())
    }
}

pub(super) struct Docker<'a, F> {
    factory: F,
    artifacts: &'a Path,
    sequence: u8,
}

impl<'a, F> Docker<'a, F>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
{
    pub fn new(factory: F, artifacts: &'a Path) -> Self {
        Self {
            factory,
            artifacts,
            sequence: 0,
        }
    }

    pub fn command(&mut self, arguments: &[String], label: &str) -> io::Result<Capture> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|value| *value <= 128)
            .ok_or_else(|| io::Error::other("OCI command count exceeded its limit"))?;
        let stem = format!("{:03}-{label}", self.sequence);
        let stdout = self.artifacts.join(format!("{stem}.stdout.txt"));
        let stderr = self.artifacts.join(format!("{stem}.stderr.txt"));
        let command = (self.factory)(arguments, &stdout, &stderr)?;
        Ok(Capture {
            command,
            stdout,
            stderr,
        })
    }

    pub fn run(
        &mut self,
        supervisor: &mut CommandSupervisor,
        arguments: &[String],
        budget: Duration,
        label: &str,
        cleanup: bool,
    ) -> io::Result<Observed> {
        self.run_with(arguments, budget, label, |command, budget| {
            if cleanup {
                supervisor.run_cleanup(command, budget)
            } else {
                supervisor.run(command, budget)
            }
        })
    }

    pub fn run_with(
        &mut self,
        arguments: &[String],
        budget: Duration,
        label: &str,
        perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<Observed> {
        let mut capture = self.command(arguments, label)?;
        let operation = perform(&mut capture.command, budget).map_err(io::Error::other);
        let observed = capture.read();
        combine(
            operation,
            observed
                .as_ref()
                .map(|_| ())
                .map_err(|error| io::Error::other(error.to_string())),
            "OCI command log preservation",
        )?;
        observed
    }
}
