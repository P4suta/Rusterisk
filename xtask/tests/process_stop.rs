#![cfg(unix)]

use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use xtask::process::{CommandSupervisor, ProcessError};

const CASE: &str = "RUSTERISK_STOP_COMMAND_CASE";

fn combine(operation: io::Result<()>, cleanup: io::Result<()>) -> io::Result<()> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn stop_service_fixture() -> io::Result<()> {
    let Some(ready) = std::env::var_os("RUSTERISK_STOP_SERVICE_READY") else {
        return Ok(());
    };
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let ready = std::path::PathBuf::from(ready);
    let pending = ready.with_extension("pending");
    std::fs::write(&pending, listener.local_addr()?.to_string())?;
    std::fs::rename(pending, ready)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                let mut request = [0; 1];
                stream.read_exact(&mut request)?;
                if request != *b"Q" {
                    return Err(io::Error::other("invalid fixture stop request"));
                }
                return if std::env::var_os("RUSTERISK_STOP_SERVICE_FAILURE").is_some() {
                    Err(io::Error::other("requested fixture service failure"))
                } else {
                    Ok(())
                };
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("fixture service deadline exceeded"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn stop_command_fixture() -> io::Result<()> {
    match std::env::var("RUSTERISK_STOP_COMMAND_ACTION").as_deref() {
        Ok("request") => {
            let address =
                std::env::var("RUSTERISK_STOP_COMMAND_ADDRESS").map_err(io::Error::other)?;
            let address = address.parse().map_err(io::Error::other)?;
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
            stream.write_all(b"Q")
        }
        Ok("failure") => Err(io::Error::other("requested fixture stop failure")),
        Ok("timeout") => {
            std::thread::sleep(Duration::from_secs(15));
            Ok(())
        }
        Ok("noop") | Err(_) => Ok(()),
        Ok(_) => Err(io::Error::other("invalid fixture stop action")),
    }
}

fn fixture(name: &str) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", name, "--nocapture"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

fn stopped(case: &str, root: &Path) -> io::Result<()> {
    let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
    let operation = (|| {
        let ready = root.join("ready");
        let mut server = fixture("stop_service_fixture")?;
        server.env("RUSTERISK_STOP_SERVICE_READY", &ready);
        if case == "service-failure" {
            server.env("RUSTERISK_STOP_SERVICE_FAILURE", "1");
        }
        let mut service = supervisor
            .start_service(&mut server, Duration::from_secs(12))
            .map_err(io::Error::other)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let address = loop {
            service.poll().map_err(io::Error::other)?;
            match std::fs::read_to_string(&ready) {
                Ok(address) => break address,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("fixture ready deadline exceeded"));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut stop = fixture("stop_command_fixture")?;
        let action = match case {
            "success" | "service-failure" | "interrupt" => "request",
            "failure" => "failure",
            "timeout" => "timeout",
            "noop" | "invalid-budget" => "noop",
            _ => return Err(io::Error::other("invalid fixture case")),
        };
        stop.env("RUSTERISK_STOP_COMMAND_ACTION", action)
            .env("RUSTERISK_STOP_COMMAND_ADDRESS", &address);
        if case == "interrupt" {
            signal_hook::low_level::raise(signal_hook::consts::SIGINT)?;
        }
        let budget = match case {
            "timeout" => Duration::from_millis(50),
            "invalid-budget" => Duration::ZERO,
            _ => Duration::from_secs(2),
        };
        let result = service.stop_with(&mut stop, budget);
        match (case, result) {
            ("success", Ok(()))
            | ("failure" | "service-failure", Err(ProcessError::Status(_)))
            | ("timeout" | "noop", Err(ProcessError::Timeout))
            | ("invalid-budget", Err(ProcessError::InvalidBudget))
            | ("interrupt", Err(ProcessError::Interrupted)) => {}
            (_, result) => {
                return Err(io::Error::other(format!(
                    "unexpected {case} stop result: {result:?}"
                )))
            }
        }
        let _listener = TcpListener::bind(address)?;
        Ok(())
    })();
    combine(operation, supervisor.finish().map_err(io::Error::other))
}

fn using_case(case: &str) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-explicit-stop-{case}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    combine(stopped(case, &root), std::fs::remove_dir_all(root))
}

#[test]
fn explicit_stop_requires_both_request_success_and_the_service_zero_exit() -> io::Result<()> {
    for case in [
        "success",
        "failure",
        "service-failure",
        "timeout",
        "noop",
        "invalid-budget",
    ] {
        using_case(case)?;
    }
    Ok(())
}

#[test]
fn interrupted_stop_owner_fixture() -> io::Result<()> {
    if std::env::var_os(CASE).is_some() {
        using_case("interrupt")?;
    }
    Ok(())
}

#[test]
fn an_interrupted_explicit_stop_still_releases_its_owned_service() -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let mut command = fixture("interrupted_stop_owner_fixture")?;
    command.env(CASE, "interrupt");
    let operation = supervisor.run(&mut command, Duration::from_secs(20));
    match (operation, supervisor.finish()) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}
