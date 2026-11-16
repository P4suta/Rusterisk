use super::{peek, Group, Supervisor};
use crate::process::ProcessError;
use rustix::process::{kill_process, waitpid, Pid, Signal, WaitOptions};
use std::{
    process::Command,
    time::{Duration, Instant},
};

fn exited(pid: Pid) -> Result<(), ProcessError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !peek(pid)? {
        if Instant::now() >= deadline {
            return Err(ProcessError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn reaped(pid: Pid) {
    assert!(matches!(
        waitpid(Some(pid), WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn terminal_group_cleanup_reaps_the_actual_command_and_anchor() -> Result<(), ProcessError> {
    let mut group = Group::new()?;
    let anchor = group.pid;
    let command = group.start(Command::new("/bin/sleep").arg("3"))?;
    assert!(group.finish()?.is_some());
    reaped(command);
    reaped(anchor);
    Ok(())
}

#[test]
fn an_anchor_that_exits_before_spawn_prevents_work_and_is_reaped() -> Result<(), ProcessError> {
    let mut group = Group::with_anchor(&mut Command::new("/usr/bin/true"))?;
    let anchor = group.pid;
    exited(anchor)?;
    assert!(group.start(&mut Command::new("/usr/bin/true")).is_err());
    assert!(group.finish()?.is_none());
    reaped(anchor);
    Ok(())
}

#[test]
fn actual_command_spawn_failure_still_reaps_the_waiting_anchor() -> Result<(), ProcessError> {
    let mut group = Group::new()?;
    let anchor = group.pid;
    let missing = std::env::temp_dir().join(format!(
        "rusterisk-missing-supervised-command-{}",
        std::process::id()
    ));
    assert!(!missing.exists());
    assert!(group.start(&mut Command::new(missing)).is_err());
    assert!(group.finish()?.is_none());
    reaped(anchor);
    Ok(())
}

#[test]
fn an_early_anchor_exit_stops_running_work_without_waiting_for_its_deadline(
) -> Result<(), ProcessError> {
    let mut supervisor = Supervisor::new()?;
    let mut group = Group::new()?;
    let anchor = group.pid;
    let command = group.start(Command::new("/bin/sleep").arg("3"))?;
    kill_process(anchor, Signal::KILL).map_err(std::io::Error::from)?;
    exited(anchor)?;
    let started = Instant::now();
    assert!(supervisor
        .monitor(command, anchor, started + Duration::from_secs(5), false)
        .is_err());
    assert!(group.finish()?.is_some());
    assert!(started.elapsed() < Duration::from_secs(2));
    supervisor.finish()?;
    reaped(command);
    reaped(anchor);
    Ok(())
}

#[test]
fn lost_child_ownership_returns_a_cleanup_failure() -> Result<(), ProcessError> {
    let mut group = Group::with_anchor(&mut Command::new("/usr/bin/true"))?;
    let anchor = group.pid;
    exited(anchor)?;
    group
        .children
        .as_mut()
        .ok_or(ProcessError::InvalidOutput)?
        .anchor
        .wait()?;
    let mut signalled = false;
    let result = group.finish_with(|_| {
        signalled = true;
        Ok(())
    });
    assert!(!signalled);
    assert!(
        matches!(result, Err(ProcessError::Cleanup { .. })),
        "{result:?}"
    );
    reaped(anchor);
    Ok(())
}

#[test]
fn group_signal_failure_keeps_the_error_and_closes_the_anchor_pipe() -> Result<(), ProcessError> {
    let mut group = Group::new()?;
    let anchor = group.pid;
    let command = group.start(&mut Command::new("/usr/bin/true"))?;
    exited(command)?;
    let mut signals = 0;
    let result = group.finish_with(|_| {
        signals += 1;
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });
    assert_eq!(signals, 1);
    assert!(
        matches!(result, Err(ProcessError::Cleanup { .. })),
        "{result:?}"
    );
    reaped(command);
    reaped(anchor);
    Ok(())
}
