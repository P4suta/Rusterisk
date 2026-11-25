#![cfg(unix)]
use rusterisk_test_support::Wait;
use std::{
    io::{self, Read},
    net::TcpListener,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use xtask::process::{CommandSupervisor, ProcessError};

#[test]
fn service_fixture() -> io::Result<()> {
    let Some(path) = std::env::var_os("RUSTERISK_SERVICE_READY") else {
        return Ok(());
    };
    let (mut reader, writer) = std::os::unix::net::UnixStream::pair()?;
    reader.set_read_timeout(Some(Wait::Shutdown.duration()))?;
    let registration =
        signal_hook::low_level::pipe::register(signal_hook::consts::SIGTERM, writer)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let path = std::path::PathBuf::from(path);
    let temporary = path.with_extension("pending");
    std::fs::write(&temporary, listener.local_addr()?.to_string())?;
    std::fs::rename(temporary, path)?;
    let result = reader.read_exact(&mut [0; 1]);
    if !signal_hook::low_level::unregister(registration) {
        return Err(io::Error::other("fixture signal cleanup failed"));
    }
    result
}

#[test]
fn an_owned_service_survives_client_commands_then_stops_and_releases_its_listener(
) -> Result<(), ProcessError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-process-service-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let ready = root.join("ready");
    let operation = (|| {
        let mut supervisor = CommandSupervisor::new()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "service_fixture", "--nocapture"])
            .env("RUSTERISK_SERVICE_READY", &ready);
        let mut service = supervisor.start_service(&mut command, Duration::from_secs(5))?;
        let address = rusterisk_test_support::file_ready(&root, || {
            service.poll().map_err(io::Error::other)?;
            match std::fs::read_to_string(&ready) {
                Ok(address) => Ok(Some(address)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        })?;
        service.run(
            Command::new(std::env::current_exe()?)
                .args(["--exact", "service_fixture"])
                .env_remove("RUSTERISK_SERVICE_READY"),
            Duration::from_secs(1),
        )?;
        service.poll()?;
        service.stop()?;
        supervisor.finish()?;
        let _rebound = TcpListener::bind(address)?;
        Ok(())
    })();
    match (operation, std::fs::remove_dir_all(&root)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
}

#[test]
fn exited_services_cannot_accept_client_commands() -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "service_fixture"])
        .env_remove("RUSTERISK_SERVICE_READY");
    let mut service = supervisor.start_service(&mut command, Duration::from_secs(3))?;
    rusterisk_test_support::child_state(|| match service.poll() {
        Err(ProcessError::InvalidOutput) => Ok(Some(())),
        Err(error) => Err(error),
        Ok(()) => Ok(None),
    })?;
    assert!(matches!(
        service.run(
            &mut Command::new("never-run-client"),
            Duration::from_secs(1)
        ),
        Err(ProcessError::InvalidOutput)
    ));
    service.stop()?;
    supervisor.finish()
}

#[test]
fn invalid_service_budgets_fail_before_starting_a_process() -> Result<(), ProcessError> {
    let mut supervisor = CommandSupervisor::new()?;
    assert!(matches!(
        supervisor.start_service(&mut Command::new("never-run-service"), Duration::ZERO),
        Err(ProcessError::InvalidBudget)
    ));
    supervisor.finish()
}

#[test]
fn expired_and_dropped_services_release_owned_listeners() -> Result<(), ProcessError> {
    for expires in [false, true] {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rusterisk-service-terminal-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root)?;
        let ready = root.join("ready");
        let operation = (|| {
            let mut supervisor = CommandSupervisor::new()?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args(["--exact", "service_fixture", "--nocapture"])
                .env("RUSTERISK_SERVICE_READY", &ready);
            let mut service = supervisor.start_service(&mut command, Duration::from_secs(3))?;
            let address = rusterisk_test_support::file_ready(&root, || {
                service.poll().map_err(io::Error::other)?;
                match std::fs::read_to_string(&ready) {
                    Ok(address) => Ok(Some(address)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                }
            })?;
            if expires {
                let mut client = Command::new(std::env::current_exe()?);
                client
                    .args(["--exact", "service_fixture"])
                    .env("RUSTERISK_SERVICE_READY", root.join("client-ready"));
                assert!(service.run(&mut client, Wait::Shutdown.duration()).is_err());
                assert!(matches!(service.poll(), Err(ProcessError::Timeout)));
                assert!(matches!(
                    service.run(
                        &mut Command::new("never-run-client"),
                        Duration::from_secs(1)
                    ),
                    Err(ProcessError::Timeout)
                ));
                service.stop()?;
            } else {
                drop(service);
            }
            supervisor.finish()?;
            let _rebound = TcpListener::bind(address)?;
            Ok(())
        })();
        match (operation, std::fs::remove_dir_all(&root)) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) => return Err(error),
            (Ok(()), Err(error)) => return Err(error.into()),
            (Err(operation), Err(cleanup)) => {
                return Err(ProcessError::Cleanup {
                    operation: operation.to_string(),
                    cleanup: cleanup.to_string(),
                });
            }
        }
    }
    Ok(())
}
