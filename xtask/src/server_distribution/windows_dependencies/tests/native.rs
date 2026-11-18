use super::super::{check_output, command};
use crate::{
    process::{CommandStream, CommandSupervisor},
    server_distribution::native::{combine, output, read, write},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn reserve_artifacts() -> io::Result<PathBuf> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let directory = parent.join(format!(
            "rusterisk-windows-modules-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&directory) {
            Ok(()) => return fs::canonicalize(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Windows module test artifact reservation capacity exceeded",
    ))
}

fn observe_and_replay(artifacts: &Path) -> io::Result<()> {
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let executable = executable
        .to_str()
        .ok_or_else(|| io::Error::other("Current executable path is not UTF-8"))?;
    let expected = artifacts.join("executable.path.txt");
    write(&expected, executable.as_bytes())?;
    let stdout = artifacts.join("dependencies.stdout.txt");
    let stderr = artifacts.join("dependencies.stderr.txt");
    let mut query = command(std::process::id())?;
    query
        .stdout(CommandStream::File(output(&stdout)?))
        .stderr(CommandStream::File(output(&stderr)?));
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = supervisor
        .run_owned(query, Duration::from_secs(15))
        .map_err(io::Error::other);
    let operation = combine(
        operation,
        supervisor.finish().map_err(io::Error::other),
        "Windows module observer supervisor cleanup",
    );
    let replay = (|| {
        let stdout = read(&stdout, 65_536)?;
        let stderr = read(&stderr, 4096)?;
        let saved = read(&expected, 4096)?;
        let saved = std::str::from_utf8(&saved).map_err(io::Error::other)?;
        assert_eq!(saved, executable);
        check_output(&stdout, &stderr, saved)?;
        check_output(
            &stdout,
            &stderr,
            saved.strip_prefix(r"\\?\").unwrap_or(saved),
        )
    })();
    combine(operation, replay, "Windows module observation replay")
}

#[test]
fn current_process_modules_replay_with_the_canonical_executable() -> io::Result<()> {
    let artifacts = reserve_artifacts()?;
    println!("Windows module test artifacts: {}", artifacts.display());
    observe_and_replay(&artifacts)
}
