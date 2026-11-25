use std::{
    process::{Command, Stdio},
    time::Duration,
};
use xtask::process::{run, ProcessError};
#[cfg(unix)]
mod process {
    pub mod descendants;
}

#[test]
fn process_fixture() -> Result<(), String> {
    match std::env::var("RUSTERISK_PROCESS_FIXTURE").as_deref() {
        Ok("failure") => Err("requested fixture failure".into()),
        Ok("timeout") => {
            std::thread::park();
            Ok(())
        }
        Ok("success") | Err(_) => Ok(()),
        #[cfg(unix)]
        Ok(
            mode @ ("tree-parent" | "tree-worker" | "tree-success" | "tree-failure" | "tree-owner"
            | "scope-repeat"),
        ) => process::descendants::fixture(mode),
        Ok(_) => Err("unknown fixture mode".into()),
    }
}

#[cfg(not(any(unix, windows)))]
#[test]
fn unsupported_process_supervision_is_explicit() {
    assert!(matches!(
        xtask::process::CommandSupervisor::new(),
        Err(ProcessError::UnsupportedSupervision)
    ));
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
