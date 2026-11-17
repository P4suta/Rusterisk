#![cfg(unix)]

use super::{verify_server_with, ServerRequest};
use crate::{
    process::CommandSupervisor,
    release_check::candidate_fixtures::{native_target, require, using},
    server_distribution::{ServerExecutionMode, ServerTarget},
};
use std::{io, process::Command, time::Duration};

#[path = "server_execution_tests.rs"]
mod execution;

#[test]
fn server_source_changes_cannot_start_native_or_sdk_commands() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        paths.change_server_source()?;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_server_with(
            captured,
            ServerRequest {
                candidate: &paths.server,
                target: paths.target,
                mode: ServerExecutionMode::Native,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |_: &mut Command, _: Duration| {
                commands += 1;
                Err(io::Error::other(
                    "unbound server inputs reached command construction",
                ))
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("unbound server returned an outcome"))?;
        require(commands == 0, "Server commands ran before source binding")?;
        require(
            error.to_string().contains("differs"),
            "Server source rejection lost its byte mismatch",
        )?;
        require(
            !paths.execution.exists()
                && !paths.consumer_execution.exists()
                && !paths.artifacts.exists(),
            "Server source rejection created runtime state",
        )
    })
}

#[test]
fn same_version_sdk_source_changes_cannot_start_server_verification() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        paths.change_sdk_source()?;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_server_with(
            captured,
            ServerRequest {
                candidate: &paths.server,
                target: paths.target,
                mode: ServerExecutionMode::Native,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |_: &mut Command, _: Duration| {
                commands += 1;
                Err(io::Error::other(
                    "unbound SDK inputs reached server command construction",
                ))
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("unbound SDK returned a server outcome"))?;
        require(
            commands == 0,
            "Server verification accepted SDK version equality without source binding",
        )?;
        require(
            error.to_string().contains("differs"),
            "SDK source rejection lost its byte mismatch",
        )?;
        require(
            !paths.execution.exists()
                && !paths.consumer_execution.exists()
                && !paths.artifacts.exists(),
            "SDK source rejection created server runtime state",
        )
    })
}

#[test]
fn a_foreign_native_target_cannot_start_sdk_preparation() -> io::Result<()> {
    let other_architecture = match native_target()? {
        ServerTarget::LinuxX86_64 => ServerTarget::LinuxArm64,
        ServerTarget::LinuxArm64 => ServerTarget::LinuxX86_64,
        ServerTarget::MacOsArm64 => ServerTarget::MacOsX86_64,
        ServerTarget::MacOsX86_64 => ServerTarget::MacOsArm64,
        ServerTarget::WindowsX86_64 => {
            return Err(io::Error::other("native fixtures require a Unix host"))
        }
    };
    for target in [ServerTarget::WindowsX86_64, other_architecture] {
        using(target, |captured, paths| {
            let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
            let mut commands = 0;
            let result = verify_server_with(
                captured,
                ServerRequest {
                    candidate: &paths.server,
                    target: paths.target,
                    mode: ServerExecutionMode::Native,
                    sdk_candidate: &paths.sdk,
                    execution: &paths.execution,
                    consumer_execution: &paths.consumer_execution,
                    artifacts: &paths.artifacts,
                },
                supervisor,
                |_: &mut Command, _: Duration| {
                    commands += 1;
                    Err(io::Error::other(
                        "foreign target reached command construction",
                    ))
                },
            );
            require(
                result.is_err(),
                "A foreign target returned a native server outcome",
            )?;
            require(commands == 0, "A foreign target started SDK preparation")?;
            require(
                !paths.execution.exists() && !paths.consumer_execution.exists(),
                "A foreign target retained owned runtime state",
            )
        })?;
    }
    Ok(())
}

#[test]
fn a_real_sdk_preparation_spawn_failure_cannot_return_a_server_outcome() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let missing = paths.sdk.with_file_name("missing-sdk-preparation-program");
        let result = verify_server_with(
            captured,
            ServerRequest {
                candidate: &paths.server,
                target: paths.target,
                mode: ServerExecutionMode::Native,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |command: &mut Command, budget: Duration| {
                commands += 1;
                require(
                    command.get_program() == "cargo",
                    "Server verification did not prepare its SDK consumer first",
                )?;
                require(
                    budget == Duration::from_secs(180),
                    "SDK preparation changed its bounded resolution budget",
                )?;
                *command = Command::new(&missing);
                Ok(())
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("failed SDK preparation returned a server outcome"))?;
        require(
            commands == 1,
            "Server verification continued after failed SDK preparation",
        )?;
        require(
            error.to_string().contains("process failed"),
            "Server verification lost its native spawn failure",
        )?;
        require(
            !paths.execution.exists() && !paths.consumer_execution.exists(),
            "SDK preparation failure retained server runtime state",
        )
    })
}
