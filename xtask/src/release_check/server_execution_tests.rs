use super::super::consumer_jobs;
use super::{verify_server_with, CommandSupervisor, ServerRequest};
use crate::{distribution::SdkConsumerPlan, process::SupervisedCommand};
use crate::{
    release_check::candidate_fixtures::{native_target, require, using},
    server_distribution::{ServerExecutionMode, ServerTarget},
};
use std::{io, time::Duration};

#[test]
fn the_windows_server_artifact_builds_the_sdk_consumer_with_one_job() {
    assert_eq!(consumer_jobs(ServerTarget::WindowsX86_64).get(), 1);
    assert_eq!(
        consumer_jobs(ServerTarget::LinuxX86_64),
        SdkConsumerPlan::DEFAULT_JOBS
    );
}

fn fails_before_runtime(
    target: ServerTarget,
    mode: ServerExecutionMode,
    expected_commands: usize,
) -> io::Result<()> {
    using(target, |captured, paths| {
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_server_with(
            captured,
            ServerRequest {
                candidate: &paths.server,
                target: paths.target,
                mode,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |command: &mut SupervisedCommand, budget: Duration| {
                commands += 1;
                require(
                    command.get_program() == "cargo" && budget == Duration::from_secs(180),
                    "Execution policy bypassed bounded SDK preparation",
                )?;
                Err(io::Error::other("stop before candidate execution"))
            },
        );
        require(
            result.is_err(),
            "Failed execution returned a server outcome",
        )?;
        require(
            commands == expected_commands,
            "Execution policy admitted the wrong candidate target",
        )?;
        require(
            !paths.execution.exists() && !paths.consumer_execution.exists(),
            "Execution policy left owned runtime state",
        )
    })
}

#[test]
fn translation_cannot_relabel_native_or_foreign_operating_system_targets() -> io::Result<()> {
    for target in [native_target()?, ServerTarget::WindowsX86_64] {
        fails_before_runtime(target, ServerExecutionMode::Rosetta, 0)?;
    }
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn intel_server_verification_requires_explicit_translation_before_sdk_preparation() -> io::Result<()>
{
    fails_before_runtime(ServerTarget::MacOsX86_64, ServerExecutionMode::Native, 0)?;
    fails_before_runtime(ServerTarget::MacOsX86_64, ServerExecutionMode::Rosetta, 1)
}
