use super::Step;
use crate::process::{scope_captured_tools, ProcessError};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
pub(super) const OUTPUT_LIMIT: u64 = 1024 * 1024;
pub(super) fn combine<T>(
    operation: io::Result<T>,
    cleanup: io::Result<()>,
    context: &str,
) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; {context}: {cleanup}"
        ))),
    }
}

pub(super) fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other("comparison output exceeds its file limit"));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "comparison output exceeded its byte limit",
        ));
    }
    Ok(bytes)
}

pub(super) fn create(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

pub(super) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub(super) struct Observed {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

impl Observed {
    pub(super) fn diagnostics(&self) -> String {
        let stdout_start = self.stdout.len().saturating_sub(4096);
        let stderr_start = self.stderr.len().saturating_sub(4096);
        format!(
            "stdout: {}; stderr: {}",
            String::from_utf8_lossy(&self.stdout[stdout_start..]),
            String::from_utf8_lossy(&self.stderr[stderr_start..]),
        )
    }

    pub(super) fn exact(&self, expected: &[u8]) -> io::Result<()> {
        if self.stdout != expected || !self.stderr.is_empty() {
            return Err(io::Error::other(format!(
                "comparison command output changed; {}",
                self.diagnostics()
            )));
        }
        Ok(())
    }

    pub(super) fn text(&self) -> io::Result<&str> {
        if !self.stderr.is_empty() {
            return Err(io::Error::other(format!(
                "comparison inspection produced diagnostics; {}",
                self.diagnostics()
            )));
        }
        std::str::from_utf8(&self.stdout).map_err(io::Error::other)
    }
}

pub(super) struct Commands<'a> {
    pub(super) directory: &'a Path,
    source: PathBuf,
    profile: PathBuf,
    sequence: u8,
    pub(super) phase: CommandPhase,
}

pub(super) enum CommandPhase {
    Operation,
    Cleanup,
}

impl<'a> Commands<'a> {
    pub(super) fn last_output(&self) -> u8 {
        self.sequence
    }

    pub(super) fn new(source: &Path, directory: &'a Path) -> io::Result<Self> {
        let profile = fs::canonicalize(directory)?.join("mise-global.toml");
        create(&profile)?.flush()?;
        Ok(Self {
            directory,
            source: fs::canonicalize(source)?,
            profile,
            sequence: 0,
            phase: CommandPhase::Operation,
        })
    }
    pub(super) fn scope(&self, command: &mut Command) -> io::Result<()> {
        scope_captured_tools(command, &self.source, &self.profile)
    }
    pub(super) fn run(
        &mut self,
        mut command: Command,
        budget: Duration,
        perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<Observed> {
        let limit = match self.phase {
            CommandPhase::Operation => 128,
            CommandPhase::Cleanup => 144,
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|sequence| *sequence <= limit)
            .ok_or_else(|| io::Error::other("comparison command capacity exhausted"))?;
        let stdout = self.directory.join(format!("{:03}.stdout", self.sequence));
        let stderr = self.directory.join(format!("{:03}.stderr", self.sequence));
        self.scope(&mut command)?;
        command
            .stdin(Stdio::null())
            .stdout(create(&stdout)?)
            .stderr(create(&stderr)?);
        let operation = perform(&mut command, budget);
        let observed = (|| {
            Ok(Observed {
                stdout: read(&stdout, OUTPUT_LIMIT)?,
                stderr: read(&stderr, OUTPUT_LIMIT)?,
            })
        })();
        match (operation, observed) {
            (Ok(()), Ok(observed)) => Ok(observed),
            (Err(operation), Ok(observed)) => Err(io::Error::other(format!(
                "{operation}; {}",
                observed.diagnostics()
            ))),
            (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
                "{operation}; comparison log preservation failed: {cleanup}"
            ))),
        }
    }
}

pub(super) fn docker(arguments: &[String]) -> Command {
    let mut command = Command::new("docker");
    command.args(arguments);
    command
}

pub(super) fn arguments<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

pub(super) fn owned_step(mut step: Step, name: &str, id: &str) -> Step {
    for argument in &mut step.arguments {
        if argument == name {
            *argument = id.into();
        }
    }
    step
}

#[cfg(test)]
mod tests {
    use super::Observed;

    #[test]
    fn failure_diagnostics_keep_the_most_recent_output_from_each_stream() {
        let mut stdout = b"Old output\n".repeat(512);
        stdout.extend_from_slice(b"Reference call was rejected\n");
        let mut stderr = b"Old diagnostic\n".repeat(512);
        stderr.extend_from_slice(b"Fixture could not close the call\n");
        let diagnostic = Observed { stdout, stderr }.diagnostics();
        assert!(diagnostic.contains("Reference call was rejected"));
        assert!(diagnostic.contains("Fixture could not close the call"));
        assert!(diagnostic.len() <= 8192 + "stdout: ; stderr: ".len());
    }
}
