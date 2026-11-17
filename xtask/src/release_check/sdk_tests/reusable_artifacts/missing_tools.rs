use super::{require, using};
use crate::{
    process::{CommandSupervisor, ProcessError},
    release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError},
};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    time::Duration,
};

const CANDIDATE: &str = "RUSTERISK_SDK_ARTIFACT_MISSING_TOOLS_CANDIDATE";

fn combine(
    operation: Result<(), ProcessError>,
    cleanup: Result<(), ProcessError>,
) -> io::Result<()> {
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(io::Error::other(error)),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; SDK artifact fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn missing_tools_worker() -> io::Result<()> {
    let Some(candidate) = std::env::var_os(CANDIDATE) else {
        return Ok(());
    };
    let candidate = PathBuf::from(candidate);
    let captured =
        CapturedReleaseSource::read(&candidate.join("source")).map_err(io::Error::other)?;
    let published = candidate.with_file_name("consumer-artifacts");
    let result: Result<SdkOutcome<'_>, SdkRunError> =
        captured.verify_sdk_artifact(&candidate, &published);
    let error = result
        .err()
        .ok_or_else(|| io::Error::other("Missing native tools returned a reusable SDK outcome"))?;
    let diagnostic = error.to_string();
    require(
        (diagnostic.contains("tool") || diagnostic.contains("process")) && !published.exists(),
        "SDK artifact production lost its tool failure or published an incomplete bundle",
    )?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(candidate.with_file_name("missing-tools-worker-completed"))?
        .write_all(b"SDK tool failure observed\n")
}

#[test]
fn missing_native_tools_cannot_publish_sdk_artifacts() -> io::Result<()> {
    using(|_, fixture| {
        let (_, module) = module_path!()
            .split_once("::")
            .ok_or_else(|| io::Error::other("SDK worker module lacks its crate prefix"))?;
        let worker = format!("{module}::missing_tools_worker");
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", &worker, "--nocapture", "--test-threads=1"])
            .env("PATH", "")
            .env(CANDIDATE, &fixture.candidate);
        let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let operation = supervisor.run(&mut command, Duration::from_secs(5));
        combine(operation, supervisor.finish())?;
        require(
            !fixture.artifacts.exists()
                && fs::read(
                    fixture
                        .candidate
                        .with_file_name("missing-tools-worker-completed"),
                )? == b"SDK tool failure observed\n",
            "SDK tool discovery published an artifact or skipped its isolated worker",
        )
    })
}
