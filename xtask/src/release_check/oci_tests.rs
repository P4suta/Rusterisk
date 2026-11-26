#![cfg(unix)]

use super::{verify_oci_with, OciRequest};
use crate::{
    process::CommandSupervisor,
    release_check::candidate_fixtures::{linux_target, require, using, Attestation},
};
use std::{fs, io, process::Command, time::Duration};

#[path = "oci_tests/reusable_artifacts.rs"]
mod reusable_artifacts;

#[test]
fn changed_server_inputs_cannot_load_a_matching_oci_archive() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        paths.change_server_source()?;
        paths.write_oci(Attestation::Legacy)?;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_oci_with(
            captured,
            OciRequest {
                candidate: &paths.server,
                target,
                archive: paths.archive()?,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |_: &mut Command, _: Duration| {
                commands += 1;
                Err(io::Error::other(
                    "unbound OCI inputs reached command construction",
                ))
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("unbound server returned an OCI outcome"))?;
        require(
            commands == 0,
            "OCI commands ran before server source binding",
        )?;
        require(
            error.to_string().contains("differs"),
            "OCI source rejection lost its byte mismatch",
        )?;
        require(
            !paths.execution.exists()
                && !paths.consumer_execution.exists()
                && !paths.artifacts.exists(),
            "OCI source rejection created runtime state",
        )
    })
}

#[test]
fn same_version_sdk_inputs_cannot_bypass_oci_source_binding() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        paths.change_sdk_source()?;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_oci_with(
            captured,
            OciRequest {
                candidate: &paths.server,
                target,
                archive: paths.archive()?,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |_: &mut Command, _: Duration| {
                commands += 1;
                Err(io::Error::other(
                    "unbound SDK inputs reached OCI command construction",
                ))
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("unbound SDK returned an OCI outcome"))?;
        require(
            commands == 0,
            "OCI verification accepted SDK version equality without source binding",
        )?;
        require(
            error.to_string().contains("differs"),
            "OCI SDK source rejection lost its byte mismatch",
        )?;
        require(
            !paths.execution.exists()
                && !paths.consumer_execution.exists()
                && !paths.artifacts.exists(),
            "OCI SDK rejection created runtime state",
        )
    })
}

#[test]
fn invalid_oci_graph_cannot_start_preparation_or_docker_commands() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        paths.write_oci(Attestation::None)?;
        fs::write(paths.archive()?, paths.empty_index_graph()?)?;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let result = verify_oci_with(
            captured,
            OciRequest {
                candidate: &paths.server,
                target,
                archive: paths.archive()?,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |_: &mut Command, _: Duration| {
                commands += 1;
                Err(io::Error::other(
                    "invalid OCI archive reached command construction",
                ))
            },
        );
        require(result.is_err(), "Invalid OCI bytes returned an outcome")?;
        require(
            commands == 0,
            "Invalid OCI bytes started preparation or Docker commands",
        )?;
        require(
            !paths.execution.exists() && !paths.consumer_execution.exists(),
            "Invalid OCI bytes retained a private working copy",
        )
    })
}

#[test]
fn a_real_sdk_preparation_spawn_failure_cannot_return_an_oci_outcome() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        use std::os::unix::fs::PermissionsExt;
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let mut commands = 0;
        let mut mount_mode = None;
        let missing = paths
            .sdk
            .with_file_name("missing-oci-sdk-preparation-program");
        let result = verify_oci_with(
            captured,
            OciRequest {
                candidate: &paths.server,
                target,
                archive: paths.archive()?,
                sdk_candidate: &paths.sdk,
                execution: &paths.execution,
                consumer_execution: &paths.consumer_execution,
                artifacts: &paths.artifacts,
            },
            supervisor,
            |command: &mut Command, budget: Duration| {
                commands += 1;
                let config = fs::symlink_metadata(paths.execution.join("config"))?;
                mount_mode = Some(config.permissions().mode() & 0o7777);
                require(
                    command.get_program() == "cargo",
                    "OCI verification did not prepare its SDK consumer first",
                )?;
                require(
                    budget == Duration::from_secs(180),
                    "OCI SDK preparation changed its resolution budget",
                )?;
                *command = Command::new(&missing);
                Ok(())
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("failed SDK preparation returned an OCI outcome"))?;
        require(
            commands == 1,
            "OCI verification continued after failed SDK preparation",
        )?;
        assert_eq!(mount_mode, Some(0o755), "OCI configuration mount mode");
        require(
            error.to_string().contains("process failed"),
            "OCI verification lost its native spawn failure",
        )?;
        require(
            !paths.execution.exists() && !paths.consumer_execution.exists(),
            "OCI SDK preparation failure retained owned state",
        )
    })
}

#[test]
fn the_container_can_traverse_its_configuration_mount_under_a_private_umask() -> io::Result<()> {
    crate::process::run(
        Command::new("sh")
            .args(["-c", "umask 077; exec \"$@\"", "sh"])
            .arg(std::env::current_exe()?)
            .args([
                "--exact",
                concat!(
                    "release_check::oci::tests::",
                    "a_real_sdk_preparation_spawn_failure_cannot_return_an_oci_outcome",
                ),
            ]),
        Duration::from_secs(30),
    )
    .map_err(io::Error::other)
}
