#![cfg(windows)]

use rusterisk_windows_job_accounting::active_processes;
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::windows::io::AsHandle,
    path::{Path, PathBuf},
    task::{Context, Poll, Waker},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::signal::windows::{ctrl_c, CtrlC};
use windows_spawn::{Child, Command, CreationFlags, DropPolicy, Job, SpawnOptions, Stdio};
use xtask::process::{
    CommandStream, CommandSupervisor, ProcessError, RunningCommand, SupervisedCommand,
};

use super::output::Output;

const MODE: &str = "RUSTERISK_OWNED_SERVICE_MODE";
const ROOT: &str = "RUSTERISK_OWNED_SERVICE_ROOT";
const OBSERVER: &str = "RUSTERISK_OWNED_SERVICE_OBSERVER";
const ADDRESS: &str = "RUSTERISK_OWNED_SERVICE_ADDRESS";
const WORKER: &str = "owned_service::owned_service_worker";
const HELPER: &str = env!("CARGO_BIN_EXE_xtask");
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const READY_BUDGET: Duration = Duration::from_secs(5);
const IO_BUDGET: Duration = Duration::from_secs(2);

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
            "{operation}; owned service cleanup failed: {cleanup}"
        ))),
    }
}

fn deadline(budget: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("Owned service fixture deadline overflow"))
}

fn reserve_directory() -> io::Result<PathBuf> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    let stamp = checked(SystemTime::now().duration_since(UNIX_EPOCH))?.as_nanos();
    for attempt in 0..32 {
        let root = parent.join(format!(
            "rusterisk-owned-service-{}-{stamp}-{attempt}",
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
        "Owned service fixture directory capacity exceeded",
    ))
}

fn publish(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let pending = root.join(format!("{name}.pending"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    file.write_all(bytes)?;
    file.flush()?;
    drop(file);
    fs::rename(pending, root.join(name))
}

fn read(root: &Path, name: &str) -> io::Result<Vec<u8>> {
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    verify(
        metadata.is_file() && metadata.len() <= 256,
        "Owned service fixture input exceeds its regular-file bound",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(257).read_to_end(&mut bytes)?;
    verify(
        bytes.len() <= 256,
        "Owned service fixture input changed beyond its byte bound",
    )?;
    Ok(bytes)
}

fn address(name: &str) -> io::Result<SocketAddr> {
    checked(checked(std::env::var(name))?.parse())
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

fn signal(listener: &mut CtrlC) -> io::Result<bool> {
    let mut context = Context::from_waker(Waker::noop());
    match listener.poll_recv(&mut context) {
        Poll::Ready(Some(())) => Ok(true),
        Poll::Pending => Ok(false),
        Poll::Ready(None) => Err(io::Error::other("Owned service signal receiver closed")),
    }
}

fn accept(listener: &TcpListener) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let deadline = deadline(READY_BUDGET)?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(IO_BUDGET))?;
                stream.set_write_timeout(Some(IO_BUDGET))?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service descendant never connected",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_marker(root: &Path, name: &str) -> io::Result<()> {
    let deadline = deadline(READY_BUDGET)?;
    loop {
        match read(root, name) {
            Ok(bytes) => return verify(bytes == b"1", "Owned service readiness marker differs"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service readiness marker deadline exceeded",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_child(child: &mut Child, budget: Duration) -> io::Result<std::process::ExitStatus> {
    let deadline = deadline(budget)?;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service fixture child deadline exceeded",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_descendant(child: &mut std::process::Child) -> io::Result<()> {
    let deadline = deadline(READY_BUDGET)?;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service descendant termination deadline exceeded",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn release_descendant(child: &mut std::process::Child) -> io::Result<()> {
    let termination = match child.kill() {
        Ok(()) => Ok(()),
        Err(error) => match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(error),
            Err(cleanup) => combine(Err(error), Err(cleanup)),
        },
    };
    let reaped = wait_descendant(child);
    combine(termination, reaped)
}

fn empty(job: &Job) -> io::Result<()> {
    let deadline = deadline(READY_BUDGET)?;
    loop {
        if active_processes(job.as_handle())? == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service fixture Job did not become empty",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn release_parent(job: &Job, child: Option<Child>) -> io::Result<()> {
    let termination = job.terminate(72);
    let reaped = match child {
        Some(mut child) => {
            let result = wait_child(&mut child, READY_BUDGET).map(|_| ());
            drop(child);
            result
        }
        None => Ok(()),
    };
    let empty = empty(job);
    combine(combine(termination, reaped), empty)
}

fn consoleless_parent(root: &Path) -> io::Result<()> {
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", WORKER, "--nocapture", "--test-threads=1"])
        .env(MODE, "parent")
        .env(ROOT, root)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let started = command.spawn_with(
        SpawnOptions::new()
            .job(&job)
            .drop_policy(DropPolicy::Detach)
            .creation_flags(CreationFlags::DETACHED_PROCESS),
    );
    drop(command);
    let (operation, child) = match started {
        Ok(mut child) => {
            let operation = wait_child(&mut child, Duration::from_secs(40)).and_then(|status| {
                verify(status.success(), "Consoleless owned service parent failed")
            });
            (operation, Some(child))
        }
        Err(error) => (Err(error), None),
    };
    combine(operation, release_parent(&job, child))
}

fn service_ready(service: &mut RunningCommand<'_>, root: &Path) -> io::Result<SocketAddr> {
    let deadline = deadline(READY_BUDGET)?;
    loop {
        checked(service.poll())?;
        match read(root, "service-ready") {
            Ok(bytes) => return checked(checked(std::str::from_utf8(&bytes))?.parse()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service did not publish readiness",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
    )
}

fn descendant_stopped(stream: &mut TcpStream) -> io::Result<()> {
    match stream.read(&mut [0; 1]) {
        Ok(0) => Ok(()),
        Err(error) if disconnected(&error) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "Owned service descendant survived its Job cleanup",
        )),
    }
}

fn exercise(
    mut service: RunningCommand<'_>,
    root: &Path,
    listener: &TcpListener,
) -> io::Result<()> {
    let mut observed = None;
    let operation = (|| {
        let address = service_ready(&mut service, root)?;
        let mut descendant = accept(listener)?;
        let mut byte = [0];
        descendant.read_exact(&mut byte)?;
        verify(byte == *b"R", "Owned service descendant readiness differs")?;
        descendant.write_all(b"P")?;
        descendant.read_exact(&mut byte)?;
        verify(
            byte == *b"A",
            "Owned service descendant is not live before stop",
        )?;
        observed = Some((address, descendant));
        let mut client = request(root, "client")?;
        checked(client.env(ADDRESS, address.to_string()))?;
        checked(service.run_owned(client, IO_BUDGET))?;
        checked(service.poll())
    })();
    let stop = checked(service.stop());
    let graceful = if operation.is_ok() && stop.is_ok() {
        read(root, "graceful-stop")
            .and_then(|bytes| verify(bytes == b"1", "Owned service did not stop through Ctrl+C"))
    } else {
        Ok(())
    };
    let released = match observed {
        Some((address, mut stream)) => {
            let descendant = descendant_stopped(&mut stream);
            let listener = TcpListener::bind(address).map(|_| ());
            combine(descendant, listener)
        }
        None => Ok(()),
    };
    combine(combine(combine(operation, stop), graceful), released)
}

fn parent(root: &Path) -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut supervisor = checked(CommandSupervisor::with_console_helper(Path::new(HELPER)))?;
    let operation = (|| {
        let mut command = request(root, "service")?;
        checked(command.env(OBSERVER, listener.local_addr()?.to_string()))?;
        let output = Output::capture(&mut command)?;
        let operation = match supervisor.start_service_owned(command, Duration::from_secs(20)) {
            Ok(service) => exercise(service, root, &listener),
            Err(error) => checked(Err::<(), ProcessError>(error)),
        };
        let diagnostics = match output.read() {
            Ok((stdout, stderr)) => {
                if operation.is_err() {
                    eprintln!("Owned service stdout:\n{stdout}");
                    eprintln!("Owned service stderr:\n{stderr}");
                }
                verify(
                    stderr.is_empty(),
                    "Owned service emitted an unexpected diagnostic",
                )
            }
            Err(error) => Err(error),
        };
        combine(operation, diagnostics)?;
        checked(supervisor.run_owned(request(root, "noop")?, IO_BUDGET))
    })();
    combine(operation, checked(supervisor.finish()))
}

fn descendant(root: &Path) -> io::Result<()> {
    let mut interruption = ctrl_c()?;
    verify(
        !signal(&mut interruption)?,
        "Owned service descendant was interrupted before readiness",
    )?;
    let mut stream = TcpStream::connect_timeout(&address(OBSERVER)?, IO_BUDGET)?;
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;
    stream.set_write_timeout(Some(IO_BUDGET))?;
    stream.write_all(b"R")?;
    publish(root, "descendant-ready", b"1")?;
    let deadline = deadline(Duration::from_secs(30))?;
    loop {
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::other(
                    "Owned service observer closed before Job cleanup",
                ))
            }
            Ok(_) => {
                verify(byte == *b"P", "Owned service descendant probe differs")?;
                stream.write_all(b"A")?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
        signal(&mut interruption)?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned service descendant outlived its cleanup budget",
            ));
        }
    }
}

fn service(root: &Path) -> io::Result<()> {
    let mut interruption = ctrl_c()?;
    verify(
        !signal(&mut interruption)?,
        "Owned service was interrupted before readiness",
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut descendant = request(root, "descendant")?;
    descendant.stderr(CommandStream::Inherit);
    let mut descendant = descendant.into_std().spawn()?;
    let operation = (|| {
        wait_marker(root, "descendant-ready")?;
        publish(
            root,
            "service-ready",
            listener.local_addr()?.to_string().as_bytes(),
        )?;
        let deadline = deadline(Duration::from_secs(30))?;
        loop {
            if signal(&mut interruption)? {
                return publish(root, "graceful-stop", b"1");
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(IO_BUDGET))?;
                    stream.set_write_timeout(Some(IO_BUDGET))?;
                    let mut byte = [0];
                    stream.read_exact(&mut byte)?;
                    verify(byte == *b"C", "Owned service client request differs")?;
                    stream.write_all(b"A")?;
                    verify(
                        stream.read(&mut byte)? == 0,
                        "Owned service client did not close its request",
                    )?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Owned service did not receive graceful shutdown",
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    })();
    let cleanup = if operation.is_err() {
        release_descendant(&mut descendant)
    } else {
        Ok(())
    };
    drop(descendant);
    combine(operation, cleanup)
}

fn client() -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address(ADDRESS)?, IO_BUDGET)?;
    stream.set_read_timeout(Some(IO_BUDGET))?;
    stream.set_write_timeout(Some(IO_BUDGET))?;
    stream.write_all(b"C")?;
    stream.shutdown(Shutdown::Write)?;
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    verify(byte == *b"A", "Owned service client response differs")?;
    verify(
        stream.read(&mut byte)? == 0,
        "Owned service client response did not finish",
    )
}

#[test]
fn a_consoleless_parent_gracefully_stops_an_owned_service_and_its_descendant() -> io::Result<()> {
    let root = reserve_directory()?;
    let operation = consoleless_parent(&root);
    combine(operation, fs::remove_dir_all(root))
}

#[test]
fn owned_service_worker() -> io::Result<()> {
    let Some(mode) = std::env::var_os(MODE) else {
        return Ok(());
    };
    let root = std::env::var_os(ROOT)
        .ok_or_else(|| io::Error::other("Owned service fixture directory is missing"))?;
    let root = Path::new(&root);
    match mode.to_str() {
        Some("parent") => parent(root),
        Some("service") => service(root),
        Some("descendant") => descendant(root),
        Some("client") => client(),
        Some("noop") => Ok(()),
        Some(_) | None => Err(io::Error::other("Owned service fixture mode is invalid")),
    }
}
