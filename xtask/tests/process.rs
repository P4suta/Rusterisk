use std::{
    process::{Command, Stdio},
    time::Duration,
};
use xtask::process::{run, ProcessError};

#[test]
fn process_fixture() -> Result<(), String> {
    match std::env::var("RUSTERISK_PROCESS_FIXTURE").as_deref() {
        Ok("failure") => Err("requested fixture failure".into()),
        Ok("timeout") => {
            std::thread::sleep(Duration::from_secs(10));
            Ok(())
        }
        Ok("success") | Err(_) => Ok(()),
        Ok(_) => Err("unknown fixture mode".into()),
    }
}

fn fixture(mode: &str) -> Result<Command, std::io::Error> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "process_fixture"])
        .env("RUSTERISK_PROCESS_FIXTURE", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[test]
fn bounded_processes_require_success_and_reap_expired_children() -> Result<(), ProcessError> {
    run(&mut fixture("success")?, Duration::from_secs(5))?;
    assert!(matches!(
        run(&mut fixture("failure")?, Duration::from_secs(5)),
        Err(ProcessError::Status(_))
    ));
    let start = std::time::Instant::now();
    assert!(matches!(
        run(&mut fixture("timeout")?, Duration::from_millis(100)),
        Err(ProcessError::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(matches!(
        run(&mut fixture("success")?, Duration::ZERO),
        Err(ProcessError::InvalidBudget)
    ));
    Ok(())
}
