use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_OUTPUT: usize = 16_384;

pub(super) struct Fixture {
    root: PathBuf,
}

pub(super) fn require(valid: bool, reason: &str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason.to_owned()))
    }
}

fn combine<T>(primary: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; release CLI cleanup failed: {cleanup}"
        ))),
    }
}

impl Fixture {
    fn create() -> io::Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for attempt in 0..32 {
            let root = parent.join(format!(
                "rusterisk-release-cli-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Release CLI fixture directory could not be reserved",
        ))
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(super) fn write(&self, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let path = self.path(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(path)
    }

    fn close(self) -> io::Result<()> {
        fs::remove_dir_all(self.root)
    }
}

pub(super) fn using<T>(operation: impl FnOnce(&Fixture) -> io::Result<T>) -> io::Result<T> {
    let fixture = Fixture::create()?;
    let primary = operation(&fixture);
    combine(primary, fixture.close())
}

fn wait(child: &mut Child, budget: Duration) -> io::Result<ExitStatus> {
    let deadline = Instant::now()
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("Release CLI child deadline overflowed"))?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Release CLI child deadline elapsed",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn stop(child: &mut Child) -> io::Result<()> {
    let killed = child.kill();
    let reaped = wait(child, Duration::from_secs(2)).map(|_| ());
    combine(killed, reaped)
}

#[cfg(unix)]
fn open_output(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_output(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new().read(true).share_mode(0).open(path)
}

fn read_output(path: &Path) -> io::Result<Vec<u8>> {
    require(
        fs::symlink_metadata(path)?.is_file(),
        "Release CLI output was not a regular file",
    )?;
    let file = open_output(path)?;
    let metadata = file.metadata()?;
    require(
        metadata.is_file() && metadata.len() <= MAX_OUTPUT as u64,
        "Release CLI output exceeded its regular-file bound",
    )?;
    let mut bytes = Vec::new();
    file.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() <= MAX_OUTPUT,
        "Release CLI output exceeded its byte bound",
    )?;
    Ok(bytes)
}

fn run(fixture: &Fixture, arguments: &[&str], directory: &Path) -> io::Result<Output> {
    let stdout_path = fixture.path("stdout");
    let stderr_path = fixture.path("stderr");
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout_path)?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stderr_path)?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .args(arguments)
        .current_dir(directory)
        .env("PATH", "")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let child = command.spawn();
    drop(command);
    let mut child = child?;
    let status = match wait(&mut child, Duration::from_secs(5)) {
        Ok(status) => Ok(status),
        Err(primary) => combine(Err(primary), stop(&mut child)),
    };
    drop(child);
    let stdout = read_output(&stdout_path);
    let stderr = read_output(&stderr_path);
    let streams = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
        (Err(stdout), Err(stderr)) => Err(io::Error::other(format!(
            "stdout read failed: {stdout}; stderr read failed: {stderr}"
        ))),
    };
    match (status, streams) {
        (Ok(status), Ok((stdout, stderr))) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        (Err(primary), Ok((stdout, stderr))) => Err(io::Error::other(format!(
            "{primary}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        ))),
        (Ok(status), Err(streams)) => Err(io::Error::other(format!(
            "Release CLI exited with {status}; output failed: {streams}"
        ))),
        (Err(primary), Err(streams)) => Err(io::Error::other(format!(
            "{primary}; release CLI output failed: {streams}"
        ))),
    }
}

pub(super) fn invoke(arguments: &[&str], directory: &Path) -> io::Result<Output> {
    using(|fixture| run(fixture, arguments, directory))
}
