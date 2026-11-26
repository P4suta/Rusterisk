use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::process::{CommandStream, SupervisedCommand};

const MAX_OUTPUT: usize = 16_384;

fn verify(valid: bool, message: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn open_exclusive(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new().read(true).share_mode(0).open(path)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn create() -> io::Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for attempt in 0..32 {
            let path = parent.join(format!(
                "rusterisk-owned-output-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Owned command output directory could not be reserved",
        ))
    }

    fn close(self) -> io::Result<()> {
        fs::remove_dir_all(self.path)
    }
}

pub(super) struct Output {
    directory: TempDir,
}

fn finish<T>(primary: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "Output operation failed: {primary}; output cleanup failed: {cleanup}"
        ))),
    }
}

fn read(path: &Path) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    verify(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Owned command output was not a regular file",
    )?;
    let file = open_exclusive(path)?;
    let metadata = file.metadata()?;
    verify(
        metadata.is_file() && metadata.len() <= MAX_OUTPUT as u64,
        "Owned command output exceeded its regular-file bound",
    )?;
    let mut bytes = Vec::new();
    file.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut bytes)?;
    verify(
        bytes.len() <= MAX_OUTPUT,
        "Owned command output exceeded its bound",
    )?;
    String::from_utf8(bytes).map_err(io::Error::other)
}

impl Output {
    pub(super) fn capture(request: &mut SupervisedCommand) -> io::Result<Self> {
        let directory = TempDir::create()?;
        let result = (|| {
            let stdout = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.path.join("stdout"))?;
            let stderr = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.path.join("stderr"))?;
            request
                .stdout(CommandStream::File(stdout))
                .stderr(CommandStream::File(stderr));
            Ok(())
        })();
        match result {
            Ok(()) => Ok(Self { directory }),
            Err(error) => finish(Err(error), directory.close()),
        }
    }

    pub(super) fn read(self) -> io::Result<(String, String)> {
        let stdout = read(&self.directory.path.join("stdout"));
        let stderr = read(&self.directory.path.join("stderr"));
        let streams = match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(stdout), Err(stderr)) => Err(io::Error::other(format!(
                "stdout read failed: {stdout}; stderr read failed: {stderr}"
            ))),
        };
        finish(streams, self.directory.close())
    }
}
