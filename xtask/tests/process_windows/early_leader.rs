#![cfg(windows)]

use rusterisk_windows_job_accounting::active_processes;
use std::{
    fs::{self, File},
    io::{self, Write},
    os::windows::io::AsHandle,
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_spawn::{Child, Command, DropPolicy, Job, SpawnOptions, Stdio};

const WORKER_DIRECTORY: &str = "RUSTERISK_EARLY_LEADER_WORKER_DIRECTORY";
const LEADER_WORKER: &str = "early_leader::leader_worker";
const DESCENDANT_WORKER: &str = "early_leader::descendant_worker";
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

fn deadline(budget: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("Windows process fixture deadline overflow"))
}

fn reserve_directory() -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let directory = std::env::temp_dir().join(format!(
            "rusterisk-early-leader-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Windows process fixture reservation capacity exceeded",
    ))
}

fn marker(directory: &Path, name: &str) -> io::Result<()> {
    File::options()
        .write(true)
        .create_new(true)
        .open(directory.join(name))?
        .write_all(b"1")
}

fn worker_command(directory: &Path, name: &str) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", name, "--nocapture"])
        .env(WORKER_DIRECTORY, directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    Ok(command)
}

fn wait_for_exit(child: &mut Child) -> io::Result<ExitStatus> {
    let deadline = deadline(WAIT_BUDGET)?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows process fixture exit deadline exceeded",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_marker(directory: &Path, name: &str) -> io::Result<()> {
    let deadline = deadline(WAIT_BUDGET)?;
    loop {
        if directory.join(name).try_exists()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Windows process fixture marker deadline exceeded: {name}"),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_count(job: &Job, expected: u32) -> io::Result<()> {
    let deadline = deadline(WAIT_BUDGET)?;
    loop {
        let count = active_processes(job.as_handle())?;
        if count == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("expected {expected} Job members, received {count}"),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn stop_and_release(job: &Job, child: Option<Child>) -> io::Result<()> {
    let termination = job.terminate(73);
    let reaped = match child {
        Some(mut child) => {
            let result = wait_for_exit(&mut child).map(|_status| ());
            drop(child);
            result
        }
        None => Ok(()),
    };
    let empty = wait_for_count(job, 0);
    combine(combine(termination, reaped), empty)
}

fn observe_live_descendant(directory: &Path, job: &Job) -> io::Result<()> {
    wait_for_count(job, 1)?;
    marker(directory, "probe")?;
    wait_for_marker(directory, "acknowledged")?;
    let count = active_processes(job.as_handle())?;
    if count != 1 {
        return Err(io::Error::other(format!(
            "descendant left the Job before cleanup: {count} members"
        )));
    }
    Ok(())
}

fn check_early_leader(directory: &Path) -> io::Result<()> {
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    let mut leader =
        worker_command(directory, LEADER_WORKER)?.spawn_with(SpawnOptions::new().job(&job))?;
    let (operation, retained) = match wait_for_exit(&mut leader) {
        Ok(status) => {
            drop(leader);
            let observed = if status.success() {
                observe_live_descendant(directory, &job)
            } else {
                Err(io::Error::other(format!(
                    "early leader worker failed: {status}"
                )))
            };
            (observed, None)
        }
        Err(error) => (Err(error), Some(leader)),
    };
    let cleanup = stop_and_release(&job, retained);
    combine(operation, cleanup)
}

#[test]
fn a_reaped_leader_leaves_a_live_descendant_until_job_cleanup() -> io::Result<()> {
    let directory = reserve_directory()?;
    let operation = check_early_leader(&directory);
    let cleanup = fs::remove_dir_all(&directory);
    combine(operation, cleanup)
}

#[test]
fn leader_worker() -> io::Result<()> {
    let Some(directory) = std::env::var_os(WORKER_DIRECTORY) else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    let mut descendant = worker_command(&directory, DESCENDANT_WORKER)?
        .spawn_with(SpawnOptions::new().drop_policy(DropPolicy::Detach))?;
    let ready = wait_for_marker(&directory, "ready");
    if ready.is_ok() {
        drop(descendant);
        return Ok(());
    }
    let stopped = descendant.kill();
    let reaped = wait_for_exit(&mut descendant).map(|_status| ());
    drop(descendant);
    combine(ready, combine(stopped, reaped))
}

#[test]
fn descendant_worker() -> io::Result<()> {
    let Some(directory) = std::env::var_os(WORKER_DIRECTORY) else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    marker(&directory, "ready")?;
    let deadline = deadline(Duration::from_secs(15))?;
    let mut acknowledged = false;
    loop {
        if !acknowledged && directory.join("probe").try_exists()? {
            marker(&directory, "acknowledged")?;
            acknowledged = true;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "descendant remained alive beyond the owned cleanup deadline",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}
