use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use xtask::process::{run, CommandSupervisor, ProcessError};

fn supervised(command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let operation = supervisor.run(command, budget);
    let cleanup = supervisor.finish();
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{operation}; supervisor cleanup failed: {cleanup}"
        ))
        .into()),
    }
}

fn command(mode: &str) -> Result<Command, std::io::Error> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "process_fixture"])
        .env("RUSTERISK_PROCESS_FIXTURE", mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

fn worker() -> Result<(), std::io::Error> {
    let address: SocketAddr = std::env::var("RUSTERISK_PROCESS_ADDRESS")
        .map_err(std::io::Error::other)?
        .parse()
        .map_err(std::io::Error::other)?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"R")?;
    std::io::stderr().write_all(b"R")?;
    std::io::stderr().flush()?;
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    if byte != *b"P" {
        return Err(std::io::Error::other("unexpected descendant probe"));
    }
    stream.write_all(b"A")?;
    stream.read_exact(&mut byte)?;
    if byte != *b"S" {
        return Err(std::io::Error::other("unexpected descendant stop"));
    }
    Ok(())
}

pub fn fixture(mode: &str) -> Result<(), String> {
    let result = match mode {
        "tree-parent" => command("tree-worker")
            .and_then(|mut command| command.spawn())
            .and_then(|mut child| child.wait())
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other("descendant fixture failed"))
                }
            }),
        "tree-worker" => worker(),
        "tree-success" | "tree-failure" => (|| {
            let mut child = command("tree-worker")?.stderr(Stdio::piped()).spawn()?;
            let mut ready = [0];
            child
                .stderr
                .take()
                .ok_or_else(|| std::io::Error::other("missing descendant readiness stream"))?
                .read_exact(&mut ready)?;
            if ready != *b"R" {
                return Err(std::io::Error::other("descendant was not ready"));
            }
            if mode == "tree-success" {
                Ok(())
            } else {
                Err(std::io::Error::other("requested leader failure"))
            }
        })(),
        "tree-owner" => interrupted_owner().map_err(std::io::Error::other),
        "scope-repeat" => repeated_scope().map_err(std::io::Error::other),
        _ => Err(std::io::Error::other("unknown descendant fixture")),
    };
    result.map_err(|error| error.to_string())
}

fn interrupted_owner() -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let operation = supervisor.run(&mut command("tree-parent")?, Duration::from_secs(10));
    let operation = match operation {
        Err(ProcessError::Interrupted) => {
            supervisor.run_cleanup(&mut command("success")?, Duration::from_secs(2))
        }
        Err(error) => Err(error),
        Ok(()) => Err(ProcessError::InvalidOutput),
    };
    let cleanup = supervisor.finish();
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{operation}; supervisor cleanup failed: {cleanup}"
        ))
        .into()),
    }
}

fn descriptors() -> Result<usize, std::io::Error> {
    std::fs::read_dir("/dev/fd")?.try_fold(0, |count, entry| entry.map(|_| count + 1))
}

fn repeated_scope() -> Result<(), ProcessError> {
    let initial = descriptors()?;
    let mut supervisor = CommandSupervisor::new()?;
    let active = descriptors()?;
    for _ in 0..5 {
        supervisor.run(&mut command("success")?, Duration::from_secs(2))?;
        assert_eq!(descriptors()?, active);
    }
    assert!(matches!(
        supervisor.run(&mut command("success")?, Duration::ZERO),
        Err(ProcessError::InvalidBudget)
    ));
    let missing =
        std::env::temp_dir().join(format!("rusterisk-missing-command-{}", std::process::id()));
    assert!(!missing.exists());
    assert!(supervisor
        .run(&mut Command::new(missing), Duration::from_secs(2))
        .is_err());
    supervisor.run(&mut command("success")?, Duration::from_secs(2))?;
    supervisor.finish()?;
    assert_eq!(descriptors()?, initial);
    Ok(())
}

fn disconnected(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

fn accept(listener: &TcpListener) -> Result<TcpStream, std::io::Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::other("descendant never connected"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ready(stream: &mut TcpStream) -> Result<(), std::io::Error> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    if byte != *b"R" {
        return Err(std::io::Error::other("descendant did not report readiness"));
    }
    Ok(())
}

fn probe_and_stop(mut stream: TcpStream) -> Result<bool, std::io::Error> {
    let mut byte = [0];
    if let Err(error) = stream.write_all(b"P") {
        return if disconnected(&error) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    match stream.read_exact(&mut byte) {
        Err(error) if disconnected(&error) => Ok(false),
        Err(error) => Err(error),
        Ok(_) => {
            if byte != *b"A" {
                return Err(std::io::Error::other("invalid descendant response"));
            }
            stream.write_all(b"S")?;
            if stream.read(&mut byte)? != 0 {
                return Err(std::io::Error::other("descendant did not stop"));
            }
            Ok(true)
        }
    }
}

#[test]
fn timeout_stops_the_command_and_its_live_descendants() -> Result<(), ProcessError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut command = command("tree-parent")?;
    command.env(
        "RUSTERISK_PROCESS_ADDRESS",
        listener.local_addr()?.to_string(),
    );
    let result = supervised(&mut command, Duration::from_millis(700));
    let mut stream = accept(&listener)?;
    ready(&mut stream)?;
    let alive = probe_and_stop(stream)?;
    assert!(matches!(result, Err(ProcessError::Timeout)), "{result:?}");
    assert!(
        !alive,
        "the descendant remained alive after its command deadline"
    );
    Ok(())
}

fn early_exit(mode: &str) -> Result<(), ProcessError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut command = command(mode)?;
    command.env(
        "RUSTERISK_PROCESS_ADDRESS",
        listener.local_addr()?.to_string(),
    );
    let result = supervised(&mut command, Duration::from_secs(5));
    let mut stream = accept(&listener)?;
    ready(&mut stream)?;
    let alive = probe_and_stop(stream)?;
    if mode == "tree-success" {
        assert!(result.is_ok(), "{result:?}");
    } else {
        assert!(matches!(result, Err(ProcessError::Status(_))), "{result:?}");
    }
    assert!(!alive, "the descendant outlived its exited command leader");
    Ok(())
}

#[test]
fn successful_leader_exit_still_stops_its_descendants() -> Result<(), ProcessError> {
    early_exit("tree-success")
}

#[test]
fn failed_leader_exit_still_stops_its_descendants() -> Result<(), ProcessError> {
    early_exit("tree-failure")
}

#[cfg(unix)]
fn interrupt(signal: &str) -> Result<(), ProcessError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut owner = command("tree-owner")?
        .env(
            "RUSTERISK_PROCESS_ADDRESS",
            listener.local_addr()?.to_string(),
        )
        .spawn()?;
    let mut stream = accept(&listener)?;
    ready(&mut stream)?;
    let operation = run(
        Command::new("/bin/kill").args([signal, &owner.id().to_string()]),
        Duration::from_secs(2),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = owner.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            owner.kill()?;
            owner.wait()?;
            return Err(ProcessError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let alive = probe_and_stop(stream)?;
    operation?;
    assert!(
        status.success(),
        "runner failed to clean up after its interrupt: {status}"
    );
    assert!(!alive, "the descendant outlived its interrupted runner");
    Ok(())
}

#[cfg(unix)]
#[test]
fn interrupting_the_runner_stops_its_owned_descendants() -> Result<(), ProcessError> {
    interrupt("-TERM")?;
    interrupt("-INT")
}

#[cfg(unix)]
#[test]
fn supervisor_reuses_and_releases_its_signal_descriptors() -> Result<(), ProcessError> {
    run(&mut command("scope-repeat")?, Duration::from_secs(5))
}
