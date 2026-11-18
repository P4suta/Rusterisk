#![cfg(windows)]

use rusterisk_windows_job_accounting::active_processes;
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::windows::io::AsHandle,
    path::{Path, PathBuf},
    process::ExitStatus,
    task::{Context, Poll, Waker},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::signal::windows::{ctrl_c, CtrlC};
use windows_spawn::{Child, Command, CreationFlags, DropPolicy, Job, SpawnOptions, Stdio};
use xtask::process::{CommandSupervisor, ProcessError, SupervisedCommand};

const MODE: &str = "RUSTERISK_OWNED_INTERRUPT_MODE";
const ROOT: &str = "RUSTERISK_OWNED_INTERRUPT_ROOT";
const WORKER: &str = "owned_interrupt::interrupt_worker";
const HELPER: &str = env!("CARGO_BIN_EXE_xtask");
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const WAIT_BUDGET: Duration = Duration::from_secs(5);
const COMMAND_BUDGET: Duration = Duration::from_secs(3);

fn checked<T>(result: Result<T, impl std::fmt::Display>) -> io::Result<T> {
    result.map_err(|error| io::Error::other(error.to_string()))
}

fn verify(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn combine<T>(operation: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; owned interruption cleanup failed: {cleanup}"
        ))),
    }
}

fn deadline(budget: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("Owned interruption fixture deadline overflow"))
}

fn reserve_directory() -> io::Result<PathBuf> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = checked(SystemTime::now().duration_since(UNIX_EPOCH))?.as_nanos();
    for attempt in 0..32 {
        let root = parent.join(format!(
            "rusterisk-owned-interrupt-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Owned interruption fixture directory capacity exceeded",
    ))
}

fn marker(root: &Path, name: &str) -> io::Result<()> {
    let pending = root.join(format!("{name}.pending"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    file.write_all(b"1")?;
    file.flush()?;
    drop(file);
    fs::rename(pending, root.join(name))
}

fn marked(root: &Path, name: &str) -> io::Result<bool> {
    let path = root.join(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    verify(
        metadata.is_file() && metadata.len() == 1,
        "Owned interruption marker exceeds its regular-file bound",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(2).read_to_end(&mut bytes)?;
    verify(bytes == b"1", "Owned interruption marker contents differ")?;
    Ok(true)
}

fn wait_marker(root: &Path, name: &str, child: &mut Child) -> io::Result<()> {
    let deadline = deadline(WAIT_BUDGET)?;
    loop {
        if marked(root, name)? {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Owned interruption worker exited before {name}: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Owned interruption marker deadline exceeded: {name}"),
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_child(child: &mut Child, budget: Duration) -> io::Result<ExitStatus> {
    let deadline = deadline(budget)?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned interruption child deadline exceeded",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn empty(job: &Job) -> io::Result<()> {
    let deadline = deadline(WAIT_BUDGET)?;
    loop {
        if active_processes(job.as_handle())? == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned interruption Job did not become empty",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn owned_child(
    command: &mut Command,
    flags: CreationFlags,
    operation: impl FnOnce(&mut Child) -> io::Result<()>,
) -> io::Result<()> {
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    let started = command.spawn_with(
        SpawnOptions::new()
            .job(&job)
            .drop_policy(DropPolicy::Detach)
            .creation_flags(flags),
    );
    let (operation, child) = match started {
        Ok(mut child) => (operation(&mut child), Some(child)),
        Err(error) => (Err(error), None),
    };
    let termination = job.terminate(74);
    let reaped = match child {
        Some(mut child) => {
            let result = wait_child(&mut child, WAIT_BUDGET).map(|_| ());
            drop(child);
            result
        }
        None => Ok(()),
    };
    let empty = empty(&job);
    combine(operation, combine(combine(termination, reaped), empty))
}

fn signal_worker(worker: &Child) -> io::Result<()> {
    verify(
        worker.id() != 0,
        "Owned interruption worker PID must be positive",
    )?;
    let helper = fs::canonicalize(HELPER)?;
    verify(
        fs::symlink_metadata(&helper)?.is_file(),
        "Owned interruption helper is not a regular executable",
    )?;
    let mut command = Command::new(helper);
    command
        .args(["signal-owned-console", &worker.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    owned_child(&mut command, CreationFlags::DETACHED_PROCESS, |helper| {
        let status = wait_child(helper, WAIT_BUDGET)?;
        verify(status.success(), "Owned console signal helper failed")
    })
}

fn request(root: &Path, mode: &str) -> io::Result<SupervisedCommand> {
    let mut request = checked(SupervisedCommand::new(std::env::current_exe()?))?;
    for argument in ["--exact", WORKER, "--nocapture", "--test-threads=1"] {
        checked(request.arg(argument))?;
    }
    checked(request.env(MODE, mode))?;
    checked(request.env(ROOT, root))?;
    checked(request.current_dir(root))?;
    Ok(request)
}

fn interrupted(result: Result<(), ProcessError>, stage: &str) -> io::Result<()> {
    match result {
        Err(ProcessError::Interrupted) => Ok(()),
        result => Err(io::Error::other(format!(
            "{stage} did not return Interrupted: {result:?}"
        ))),
    }
}

fn reject_commands(supervisor: &mut CommandSupervisor, root: &Path) -> io::Result<()> {
    for _ in 0..2 {
        interrupted(
            supervisor.run_owned(request(root, "ordinary")?, COMMAND_BUDGET),
            "Ordinary command in an interrupted scope",
        )?;
    }
    verify(
        !marked(root, "ordinary-ran")?,
        "An interrupted scope spawned an ordinary command",
    )
}

fn wait_signal(observer: &mut CtrlC) -> io::Result<()> {
    let deadline = deadline(WAIT_BUDGET)?;
    let mut context = Context::from_waker(Waker::noop());
    loop {
        match observer.poll_recv(&mut context) {
            Poll::Ready(Some(())) => return Ok(()),
            Poll::Pending => {}
            Poll::Ready(None) => {
                return Err(io::Error::other("Owned interruption observer closed"))
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned interruption signal delivery deadline exceeded",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn fresh_scope(root: &Path) -> io::Result<()> {
    let mut fresh = checked(CommandSupervisor::new())?;
    let operation = request(root, "fresh")
        .and_then(|request| checked(fresh.run_owned(request, COMMAND_BUDGET)));
    let finished = checked(fresh.finish());
    let completed = marked(root, "fresh-ran")
        .and_then(|marked| verify(marked, "Fresh interruption scope did not execute"));
    combine(combine(operation, finished), completed)
}

fn scopes(root: &Path) -> io::Result<()> {
    let mut first = checked(CommandSupervisor::new())?;
    let mut second = checked(CommandSupervisor::new())?;
    let mut cleanup = checked(CommandSupervisor::new())?;
    let mut observer = ctrl_c()?;
    let mut delivered = false;
    let operation = (|| {
        marker(root, "ready")?;
        wait_signal(&mut observer)?;
        delivered = true;
        marker(root, "acknowledged")?;
        let first_rejected = reject_commands(&mut first, root);
        let second_rejected = reject_commands(&mut second, root);
        let cleaned = request(root, "cleanup")
            .and_then(|request| checked(cleanup.run_cleanup_owned(request, COMMAND_BUDGET)));
        let completed = combine(combine(first_rejected, second_rejected), cleaned);
        if completed.is_ok() {
            let fresh = fresh_scope(root);
            let first = reject_commands(&mut first, root);
            let second = reject_commands(&mut second, root);
            combine(combine(completed, fresh), combine(first, second))
        } else {
            completed
        }
    })();
    let first = checked(first.finish());
    let second = checked(second.finish());
    let cleanup = if delivered {
        interrupted(cleanup.finish(), "Cleanup-only scope finish")
    } else {
        checked(cleanup.finish())
    };
    let completed = combine(combine(combine(operation, first), second), cleanup);
    let completed = if completed.is_ok() {
        combine(
            completed,
            marked(root, "cleanup-ran")
                .and_then(|marked| verify(marked, "Interrupted cleanup command did not execute")),
        )
    } else {
        completed
    };
    drop(observer);
    completed
}

#[test]
fn concurrent_interruption_scopes_latch_allow_cleanup_and_leave_fresh_scopes_quiet(
) -> io::Result<()> {
    let root = reserve_directory()?;
    let operation = (|| {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", WORKER, "--nocapture", "--test-threads=1"])
            .env(MODE, "scopes")
            .env(ROOT, &root)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        owned_child(&mut command, CreationFlags::NEW_CONSOLE, |worker| {
            wait_marker(&root, "ready", worker)?;
            signal_worker(worker)?;
            wait_marker(&root, "acknowledged", worker)?;
            let status = wait_child(worker, Duration::from_secs(15))?;
            verify(status.success(), "Owned interruption scope worker failed")
        })
    })();
    combine(operation, fs::remove_dir_all(root))
}

#[test]
fn interrupt_worker() -> io::Result<()> {
    let Some(mode) = std::env::var_os(MODE) else {
        return Ok(());
    };
    let root = std::env::var_os(ROOT)
        .ok_or_else(|| io::Error::other("Owned interruption fixture directory is missing"))?;
    let root = Path::new(&root);
    match mode.to_str() {
        Some("scopes") => scopes(root),
        Some("ordinary") => marker(root, "ordinary-ran"),
        Some("cleanup") => marker(root, "cleanup-ran"),
        Some("fresh") => marker(root, "fresh-ran"),
        Some(_) | None => Err(io::Error::other(
            "Owned interruption fixture mode is invalid",
        )),
    }
}
