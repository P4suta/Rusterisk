use super::ShutdownSignal;
use rusterisk_test_support::Wait;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const DIRECTORY: &str = "RUSTERISK_SHUTDOWN_SIGNAL_WORKER_DIRECTORY";
const WORKER: &str = "shutdown_signal_tests::shutdown_registration_worker";

fn combine<T>(operation: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; signal fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn marker(directory: &Path, name: &str) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))?
        .write_all(b"1")
}

fn wait(child: &mut Child) -> io::Result<ExitStatus> {
    rusterisk_test_support::child_exit(child, Wait::Ready)
}

fn stop(child: &mut Child) -> io::Result<()> {
    let termination = child.kill();
    let reaped = wait(child).map(|_| ());
    combine(termination, reaped)
}

fn close(child: &mut Child) -> io::Result<()> {
    match child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => stop(child),
        Err(error) => combine(Err(error), stop(child)),
    }
}

fn send(signal: &str, process: u32) -> io::Result<()> {
    let mut child = Command::new("/bin/kill")
        .arg(signal)
        .arg(process.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let operation = wait(&mut child).and_then(|status| {
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Shutdown signal delivery failed: {status}"
            )))
        }
    });
    combine(operation, close(&mut child))
}

fn ready(child: &mut Child, directory: &Path) -> io::Result<()> {
    rusterisk_test_support::file_ready(directory, || {
        if directory.join("ready").try_exists()? {
            return Ok(Some(()));
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Shutdown worker exited before readiness: {status}"
            )));
        }
        Ok(None)
    })
}

fn exercise(signal: &str, directory: &Path) -> io::Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", WORKER, "--test-threads=1", "--nocapture"])
        .env(DIRECTORY, directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let operation = (|| {
        ready(&mut child, directory)?;
        send(signal, child.id())?;
        marker(directory, "release")?;
        let status = wait(&mut child)?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Shutdown receiver was not registered before its first wait for {signal}: {status}"
            )))
        }
    })();
    combine(operation, close(&mut child))
}

fn reserve_directory() -> io::Result<PathBuf> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..32 {
        let directory = parent.join(format!(
            "rusterisk-shutdown-registration-{}-{stamp}-{attempt}",
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
        "Shutdown signal fixture directory capacity exceeded",
    ))
}

#[test]
fn registered_shutdown_signals_receive_delivery_before_the_first_wait() -> io::Result<()> {
    let mut result = Ok(());
    for signal in ["-INT", "-TERM"] {
        let case = (|| {
            let directory = reserve_directory()?;
            combine(exercise(signal, &directory), fs::remove_dir_all(directory))
        })();
        result = combine(result, case);
    }
    result
}

#[tokio::test]
async fn shutdown_registration_worker() -> io::Result<()> {
    let Some(directory) = std::env::var_os(DIRECTORY) else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    let mut shutdown = ShutdownSignal::register()?;
    marker(&directory, "ready")?;
    let mut changes = rusterisk_test_support::DirectoryChanges::new(&directory)?;
    Wait::Ready
        .bounded(async {
            while !directory.join("release").try_exists()? {
                changes.changed().await?;
            }
            Ok::<(), io::Error>(())
        })
        .await
        .map_err(io::Error::other)??;
    Wait::Ready
        .bounded(shutdown.recv())
        .await
        .map_err(|_| io::Error::other("Registered shutdown signal was lost before waiting"))??;
    Ok(())
}
