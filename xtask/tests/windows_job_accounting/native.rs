use rusterisk_windows_job_accounting::active_processes;
use std::{
    fs::File,
    io::{self, Write},
    os::windows::io::AsHandle,
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_spawn::{Child, Command, Job, SpawnOptions, Stdio};

const WORKER_DIRECTORY: &str = "RUSTERISK_JOB_ACCOUNTING_WORKER_DIRECTORY";
const WAIT_BUDGET: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

fn combine(operation: io::Result<()>, cleanup: io::Result<()>) -> io::Result<()> {
    match (operation, cleanup) {
        (Ok(()), result) | (result, Ok(())) => result,
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; cleanup failed: {cleanup}"
        ))),
    }
}

fn reserve_directory() -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let directory = std::env::temp_dir().join(format!(
            "rusterisk-job-accounting-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Job accounting fixture reservation capacity exceeded",
    ))
}

fn marker(directory: &Path, name: &str) -> io::Result<()> {
    File::options()
        .write(true)
        .create_new(true)
        .open(directory.join(name))?
        .write_all(b"1")
}

fn wait_for_ready(directory: &Path, child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        if directory.join("ready").try_exists()? {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Job accounting worker exited before readiness: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Job accounting worker readiness deadline exceeded",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_exit(child: &mut Child) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Job accounting worker exit deadline exceeded",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_empty(job: &Job) -> io::Result<()> {
    let deadline = Instant::now() + WAIT_BUDGET;
    loop {
        if active_processes(job.as_handle())? == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Job accounting empty observation deadline exceeded",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn stop_and_release(job: &Job, mut child: Child) -> io::Result<()> {
    let termination = job.terminate(71);
    let reaped = wait_for_exit(&mut child).map(|_status| ());
    drop(child);
    let empty = wait_for_empty(job);
    combine(combine(termination, reaped), empty)
}

fn check_lifecycle(directory: &Path) -> io::Result<()> {
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    let empty = active_processes(job.as_handle())?;
    if empty != 0 {
        return Err(io::Error::other(format!(
            "expected an empty Job, received {empty}"
        )));
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "fixtures::accounting_worker", "--nocapture"])
        .env(WORKER_DIRECTORY, directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = command.spawn_with(SpawnOptions::new().job(&job))?;
    let operation = (|| {
        wait_for_ready(directory, &mut child)?;
        let count = active_processes(job.as_handle())?;
        if count != 1 {
            return Err(io::Error::other(format!(
                "expected one live Job member, received {count}"
            )));
        }
        marker(directory, "release")?;
        let status = wait_for_exit(&mut child)?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "Job accounting worker failed: {status}"
            )));
        }
        Ok(())
    })();
    combine(operation, stop_and_release(&job, child))
}

#[test]
fn native_query_observes_empty_live_and_reaped_job_states() -> io::Result<()> {
    let directory = reserve_directory()?;
    let operation = check_lifecycle(&directory);
    combine(operation, std::fs::remove_dir_all(&directory))
}

#[test]
fn native_query_rejects_a_non_job_handle() -> io::Result<()> {
    let file = File::open("NUL")?;
    assert!(active_processes(file.as_handle()).is_err());
    Ok(())
}

#[test]
fn accounting_worker() -> io::Result<()> {
    let Some(directory) = std::env::var_os(WORKER_DIRECTORY) else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    marker(&directory, "ready")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if directory.join("release").try_exists()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Job accounting worker release deadline exceeded",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}
