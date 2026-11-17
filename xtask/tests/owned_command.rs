#![cfg(unix)]

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use xtask::process::{CommandStream, CommandSupervisor, ProcessError, SupervisedCommand};

const MODE: &str = "RUSTERISK_OWNED_COMMAND_FIXTURE";
const PRIVATE: &str = "RUSTERISK_OWNED_COMMAND_PRIVATE";
const SET: &str = "RUSTERISK_OWNED_COMMAND_SET";
const DISCARDED: &str = "RUSTERISK_OWNED_COMMAND_DISCARDED";
const DIRECTORY: &str = "RUSTERISK_OWNED_COMMAND_DIRECTORY";
const ADDRESS: &str = "RUSTERISK_OWNED_COMMAND_ADDRESS";
const READY: &str = "RUSTERISK_OWNED_COMMAND_READY";
const ARGUMENT: &str = "argument with spaces ; $(literal) \"quote\"";

fn checked<T>(result: Result<T, impl std::fmt::Display>) -> io::Result<T> {
    result.map_err(|error| io::Error::other(error.to_string()))
}

fn verify(condition: bool, message: &'static str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn combine(
    operation: Result<(), ProcessError>,
    cleanup: Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

fn supervised(
    operation: impl FnOnce(&mut CommandSupervisor) -> Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let result = operation(&mut supervisor);
    combine(result, supervisor.finish())
}

fn within_directory(
    operation: impl FnOnce(&Path) -> Result<(), ProcessError>,
) -> Result<(), ProcessError> {
    let stamp = checked(SystemTime::now().duration_since(UNIX_EPOCH))?.as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "rusterisk-owned-command-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let result = std::fs::canonicalize(&directory)
        .map_err(ProcessError::from)
        .and_then(|path| operation(&path));
    combine(
        result,
        std::fs::remove_dir_all(directory).map_err(ProcessError::from),
    )
}

fn request(mode: &str) -> io::Result<SupervisedCommand> {
    let mut request = checked(SupervisedCommand::new(std::env::current_exe()?))?;
    for argument in [
        "--exact",
        "owned_command_worker",
        "--nocapture",
        "--test-threads=1",
    ] {
        checked(request.arg(argument))?;
    }
    checked(request.env(MODE, mode))?;
    Ok(request)
}

struct Output {
    stdout: UnixStream,
    stderr: UnixStream,
}

fn output_pair() -> io::Result<(UnixStream, CommandStream)> {
    let (reader, writer) = UnixStream::pair()?;
    reader.set_read_timeout(Some(Duration::from_millis(250)))?;
    Ok((
        reader,
        CommandStream::File(File::from(OwnedFd::from(writer))),
    ))
}

impl Output {
    fn capture(request: &mut SupervisedCommand) -> io::Result<Self> {
        let (stdout, output) = output_pair()?;
        let (stderr, diagnostic) = output_pair()?;
        request.stdout(output).stderr(diagnostic);
        Ok(Self { stdout, stderr })
    }

    fn read(self) -> io::Result<(String, String)> {
        fn read(stream: UnixStream) -> io::Result<String> {
            let mut bytes = Vec::new();
            stream.take(16_385).read_to_end(&mut bytes)?;
            verify(
                bytes.len() <= 16_384,
                "Owned command output exceeded its bound",
            )?;
            checked(String::from_utf8(bytes))
        }
        let stdout = read(self.stdout);
        let stderr = read(self.stderr);
        match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
            (Err(stdout), Err(stderr)) => Err(io::Error::other(format!(
                "stdout read failed: {stdout}; stderr read failed: {stderr}"
            ))),
        }
    }
}

fn accept(listener: &TcpListener) -> io::Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned descendant never connected",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn address(name: &str) -> io::Result<SocketAddr> {
    checked(checked(std::env::var(name))?.parse())
}

fn descendant() -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address(ADDRESS)?, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"R")?;
    let mut ready = TcpStream::connect_timeout(&address(READY)?, Duration::from_secs(2))?;
    ready.write_all(b"R")?;
    drop(ready);
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    verify(byte == *b"P", "Invalid owned descendant probe")?;
    stream.write_all(b"A")?;
    stream.read_exact(&mut byte)?;
    verify(byte == *b"S", "Invalid owned descendant stop")
}

fn leader(mode: &str) -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let mut descendant = request("descendant")?;
    checked(descendant.env(READY, listener.local_addr()?.to_string()))?;
    let mut descendant = descendant.into_std().spawn()?;
    let mut ready = accept(&listener)?;
    let mut byte = [0];
    ready.read_exact(&mut byte)?;
    verify(byte == *b"R", "Owned descendant did not report readiness")?;
    match mode {
        "waiting-leader" => verify(
            descendant.wait()?.success(),
            "Owned descendant failed before its deadline",
        ),
        "successful-leader" => Ok(()),
        "failed-leader" => Err(io::Error::other("Requested owned leader failure")),
        _ => Err(io::Error::other("Invalid owned leader mode")),
    }
}

fn environment(mode: &str) -> Result<(), ProcessError> {
    verify(
        std::env::var_os(PRIVATE).as_deref() == Some(OsStr::new("private input")),
        "Owned parent lost its private environment input",
    )?;
    let mut child = request("environment-leaf")?;
    checked(child.env(DISCARDED, "discarded input"))?;
    match mode {
        "remove-parent" => {
            checked(child.env_remove(PRIVATE))?;
            checked(child.env_remove(DISCARDED))?;
        }
        "clear-parent" => {
            checked(child.env_clear())?;
            checked(child.env(MODE, "environment-leaf"))?;
        }
        _ => return Err(io::Error::other("Invalid owned environment mode").into()),
    }
    checked(child.env(SET, "retained input"))?;
    supervised(|supervisor| supervisor.run_owned(child, Duration::from_secs(2)))
}

#[test]
fn owned_command_worker() -> Result<(), ProcessError> {
    let Some(mode) = std::env::var_os(MODE) else {
        return Ok(());
    };
    match mode.to_str() {
        Some("capture") => {
            verify(
                std::env::args().any(|argument| argument == ARGUMENT),
                "Owned command argument changed",
            )?;
            verify(
                std::env::var_os(PRIVATE).as_deref() == Some(OsStr::new("private input")),
                "Owned command environment changed",
            )?;
            let expected = std::env::var_os(DIRECTORY)
                .ok_or_else(|| io::Error::other("Missing owned command directory"))?;
            verify(
                std::env::current_dir()? == Path::new(&expected),
                "Owned command directory changed",
            )?;
            let mut byte = [0];
            verify(
                io::stdin().read(&mut byte)? == 0,
                "Owned command stdin was not null",
            )?;
            writeln!(io::stdout(), "owned stdout marker")?;
            writeln!(io::stderr(), "owned stderr marker")?;
            Ok(())
        }
        Some(mode @ ("remove-parent" | "clear-parent")) => environment(mode),
        Some("environment-leaf") => {
            verify(
                std::env::var_os(PRIVATE).is_none() && std::env::var_os(DISCARDED).is_none(),
                "Owned command retained a removed environment value",
            )?;
            verify(
                std::env::var_os(SET).as_deref() == Some(OsStr::new("retained input")),
                "Owned command discarded an independent environment value",
            )?;
            Ok(())
        }
        Some("failure") => Err(io::Error::other("Requested owned command failure").into()),
        Some("cleanup") => {
            writeln!(io::stdout(), "owned cleanup marker")?;
            writeln!(io::stderr(), "owned cleanup diagnostic")?;
            Ok(())
        }
        Some("interrupt-owner") => supervised(|supervisor| {
            signal_hook::low_level::raise(signal_hook::consts::SIGINT)?;
            let operation = supervisor.run_owned(request("failure")?, Duration::from_secs(2));
            let mut cleanup = request("cleanup")?;
            let output = Output::capture(&mut cleanup)?;
            let cleanup = supervisor.run_cleanup_owned(cleanup, Duration::from_secs(2));
            let streams = output
                .read()
                .and_then(|(stdout, stderr)| {
                    verify(
                        stdout.contains("owned cleanup marker")
                            && stderr == "owned cleanup diagnostic\n",
                        "Owned cleanup did not execute after interruption",
                    )
                })
                .map_err(ProcessError::from);
            let expected = verify(
                matches!(operation, Err(ProcessError::Interrupted)),
                "Owned command lost its primary interrupt",
            )
            .map_err(ProcessError::from);
            combine(combine(expected, cleanup), streams)
        }),
        Some("descendant") => descendant().map_err(ProcessError::from),
        Some(mode @ ("waiting-leader" | "successful-leader" | "failed-leader")) => {
            leader(mode).map_err(ProcessError::from)
        }
        _ => Err(io::Error::other("Invalid owned command worker mode").into()),
    }
}

#[test]
fn owned_commands_move_output_files_and_preserve_arguments_environment_and_directory(
) -> Result<(), ProcessError> {
    within_directory(|directory| {
        let mut command = request("capture")?;
        for argument in ["--skip", ARGUMENT] {
            checked(command.arg(argument))?;
        }
        checked(command.env(PRIVATE, "private input"))?;
        checked(command.env(DIRECTORY, directory))?;
        checked(command.current_dir(directory))?;
        let output = Output::capture(&mut command)?;
        let result = supervised(|supervisor| supervisor.run_owned(command, Duration::from_secs(5)));
        let streams = (|| {
            let (stdout, stderr) = output.read()?;
            verify(
                stdout.contains("owned stdout marker"),
                "Owned stdout was not delivered",
            )?;
            verify(
                stderr == "owned stderr marker\n",
                "Owned stderr was not delivered",
            )
        })()
        .map_err(ProcessError::from);
        combine(result, streams)
    })
}

#[test]
fn owned_commands_apply_environment_removal_and_clear_without_global_mutation(
) -> Result<(), ProcessError> {
    for mode in ["remove-parent", "clear-parent"] {
        let mut command = request(mode)?;
        checked(command.env(PRIVATE, "private input"))?;
        supervised(|supervisor| supervisor.run_owned(command, Duration::from_secs(5)))?;
    }
    Ok(())
}

#[test]
fn owned_failure_preserves_the_primary_status_and_runs_owned_cleanup() -> Result<(), ProcessError> {
    let mut operation = request("failure")?;
    let operation_output = Output::capture(&mut operation)?;
    let mut cleanup = request("cleanup")?;
    let cleanup_output = Output::capture(&mut cleanup)?;
    let result = supervised(|supervisor| {
        let operation = supervisor.run_owned(operation, Duration::from_secs(2));
        let cleanup = supervisor.run_cleanup_owned(cleanup, Duration::from_secs(2));
        combine(operation, cleanup)
    });
    let primary_streams = operation_output.read();
    let cleanup_streams = cleanup_output.read();
    let expected = verify(
        matches!(result, Err(ProcessError::Status(_))),
        "Owned cleanup replaced the primary command failure",
    )
    .map_err(ProcessError::from);
    let cleanup_streams = cleanup_streams
        .and_then(|(stdout, stderr)| {
            verify(
                stdout.contains("owned cleanup marker"),
                "Owned cleanup did not run",
            )?;
            verify(
                stderr == "owned cleanup diagnostic\n",
                "Owned cleanup diagnostic was not delivered",
            )
        })
        .map_err(ProcessError::from);
    combine(
        combine(
            expected,
            primary_streams.map(|_| ()).map_err(ProcessError::from),
        ),
        cleanup_streams,
    )
}

#[test]
fn rejected_owned_commands_release_output_files_before_spawning() -> Result<(), ProcessError> {
    within_directory(|directory| {
        for invalid_budget in [false, true] {
            let mut command = if invalid_budget {
                request("failure")?
            } else {
                checked(SupervisedCommand::new(directory.join("missing-program")))?
            };
            let output = Output::capture(&mut command)?;
            let budget = if invalid_budget {
                Duration::ZERO
            } else {
                Duration::from_secs(2)
            };
            let result = supervised(|supervisor| supervisor.run_owned(command, budget));
            let streams = output.read();
            let expected = verify(
                matches!(&result, Err(ProcessError::InvalidBudget)) && invalid_budget
                    || matches!(&result, Err(ProcessError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
                        && !invalid_budget,
                "Owned command changed its pre-spawn rejection",
            ).map_err(ProcessError::from);
            let streams = streams
                .and_then(|(stdout, stderr)| {
                    verify(
                        stdout.is_empty() && stderr.is_empty(),
                        "Rejected owned command produced output",
                    )
                })
                .map_err(ProcessError::from);
            combine(expected, streams)?;
        }
        Ok(())
    })
}

#[test]
fn owned_cleanup_remains_available_after_a_primary_interrupt() -> Result<(), ProcessError> {
    supervised(|supervisor| {
        supervisor.run_owned(request("interrupt-owner")?, Duration::from_secs(5))
    })
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

fn descendant_stopped(listener: &TcpListener) -> io::Result<()> {
    let mut stream = accept(listener)?;
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    verify(byte == *b"R", "Owned descendant was never ready")?;
    if let Err(error) = stream.write_all(b"P") {
        return if disconnected(&error) {
            Ok(())
        } else {
            Err(error)
        };
    }
    match stream.read_exact(&mut byte) {
        Err(error) if disconnected(&error) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => {
            verify(byte == *b"A", "Owned descendant returned an invalid probe")?;
            stream.write_all(b"S")?;
            verify(
                stream.read(&mut byte)? == 0,
                "Owned descendant did not stop",
            )?;
            Err(io::Error::other("Owned descendant outlived its command"))
        }
    }
}

#[test]
fn owned_timeout_and_early_leader_exit_stop_live_descendants() -> Result<(), ProcessError> {
    for mode in ["waiting-leader", "successful-leader", "failed-leader"] {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let mut command = request(mode)?;
        checked(command.env(ADDRESS, listener.local_addr()?.to_string()))?;
        let output = Output::capture(&mut command)?;
        let budget = if mode == "waiting-leader" {
            Duration::from_millis(700)
        } else {
            Duration::from_secs(5)
        };
        let result = supervised(|supervisor| supervisor.run_owned(command, budget));
        let descendant = descendant_stopped(&listener).map_err(ProcessError::from);
        let streams = output.read().map_err(ProcessError::from);
        let expected = match mode {
            "waiting-leader" => matches!(result, Err(ProcessError::Timeout)),
            "successful-leader" => result.is_ok(),
            "failed-leader" => matches!(result, Err(ProcessError::Status(_))),
            _ => return Err(ProcessError::InvalidOutput),
        };
        let result = verify(expected, "Owned command changed its terminal result")
            .map_err(ProcessError::from);
        combine(combine(result, descendant), streams.map(|_| ()))?;
    }
    Ok(())
}
