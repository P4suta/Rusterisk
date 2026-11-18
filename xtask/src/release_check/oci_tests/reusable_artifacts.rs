#![cfg(unix)]

use super::{verify_oci_with, OciRequest};
use crate::{
    process::CommandSupervisor,
    release_check::{
        candidate_fixtures::{linux_target, require, using, Attestation},
        candidate_run::checksum,
        OciOutcome, OciRunError,
    },
};
use std::{fs, io, process::Command, time::Duration};

#[test]
fn changed_server_inputs_cannot_publish_reusable_oci_artifacts() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        paths.change_server_source()?;
        paths.write_oci(Attestation::None)?;
        let result: Result<OciOutcome<'_>, OciRunError> = captured.verify_oci_artifact(
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Unbound server inputs published an OCI artifact"))?;
        require(
            error.to_string().contains("differs") && !paths.artifacts.exists(),
            "OCI artifact production lost server source binding or published rejected inputs",
        )
    })
}

#[test]
fn same_version_sdk_changes_cannot_publish_reusable_oci_artifacts() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        paths.change_sdk_source()?;
        let result: Result<OciOutcome<'_>, OciRunError> = captured.verify_oci_artifact(
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Unbound SDK inputs published an OCI artifact"))?;
        require(
            error.to_string().contains("differs") && !paths.artifacts.exists(),
            "OCI artifact production accepted SDK version equality without source binding",
        )
    })
}

#[test]
fn an_invalid_oci_graph_cannot_publish_a_reusable_runtime_artifact() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        let archive = paths.archive()?;
        let bytes = paths.empty_index_graph()?;
        fs::write(archive, &bytes)?;
        let result: Result<OciOutcome<'_>, OciRunError> = captured.verify_oci_artifact(
            &paths.server,
            target,
            archive,
            &paths.sdk,
            &paths.artifacts,
        );
        require(
            result.is_err() && !paths.artifacts.exists() && fs::read(archive)? == bytes,
            "OCI artifact production accepted or changed an invalid graph",
        )
    })
}

#[test]
fn invalid_sdk_archives_cannot_publish_reusable_oci_artifacts() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        let archive = paths.sdk.join("rusterisk-client-0.1.0.crate");
        fs::write(&archive, b"invalid SDK archive\n")?;
        let result: Result<OciOutcome<'_>, OciRunError> = captured.verify_oci_artifact(
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
            &paths.artifacts,
        );
        require(
            result.is_err()
                && !paths.artifacts.exists()
                && fs::read(archive)? == b"invalid SDK archive\n",
            "OCI artifact production accepted or changed an invalid SDK candidate",
        )
    })
}

#[test]
fn oci_artifact_production_preserves_an_existing_destination() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        fs::create_dir(&paths.artifacts)?;
        let marker = paths.artifacts.join("foreign");
        fs::write(&marker, b"existing OCI artifact\n")?;
        let result: Result<OciOutcome<'_>, OciRunError> = captured.verify_oci_artifact(
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("OCI producer reused an existing artifact path"))?;
        require(
            error.to_string().contains("destination")
                && fs::read(marker)? == b"existing OCI artifact\n",
            "OCI producer changed a destination outside its custody",
        )
    })
}

#[test]
fn missing_and_partial_oci_artifacts_return_no_outcome() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        let missing: Result<OciOutcome<'_>, OciRunError> = captured.read_oci_artifact(
            &paths.artifacts,
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
        );
        require(
            missing.is_err() && !paths.artifacts.exists(),
            "Missing OCI artifacts returned an outcome or created a bundle",
        )?;
        fs::create_dir(&paths.artifacts)?;
        let output = paths.artifacts.join("001-version.stdout.txt");
        fs::write(&output, b"rusterisk 0.1.0\n")?;
        let partial: Result<OciOutcome<'_>, OciRunError> = captured.read_oci_artifact(
            &paths.artifacts,
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
        );
        require(
            partial.is_err() && fs::read(output)? == b"rusterisk 0.1.0\n",
            "Partial OCI native outputs returned an outcome or changed during rejection",
        )
    })
}

#[test]
fn failed_native_preparation_diagnostics_cannot_be_imported_as_an_oci_outcome() -> io::Result<()> {
    let target = linux_target()?;
    using(target.server_target(), |captured, paths| {
        let supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let missing = paths.sdk.with_file_name("missing-oci-artifact-sdk-program");
        let mut commands = 0;
        let operation = verify_oci_with(
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
            |command: &mut Command, _: Duration| {
                commands += 1;
                require(
                    command.get_program() == "cargo",
                    "OCI diagnostic fixture skipped SDK preparation",
                )?;
                *command = Command::new(&missing);
                Ok(())
            },
        );
        require(
            operation.is_err() && commands == 1,
            "OCI preparation failure returned an outcome or continued execution",
        )?;
        let preserved = paths.artifacts.join("image.oci.tar");
        let before = checksum(&preserved, 512 * 1024 * 1024)?;
        require(
            before == checksum(paths.archive()?, 512 * 1024 * 1024)?,
            "OCI diagnostic preservation changed its checked archive bytes",
        )?;
        let imported: Result<OciOutcome<'_>, OciRunError> = captured.read_oci_artifact(
            &paths.artifacts,
            &paths.server,
            target,
            paths.archive()?,
            &paths.sdk,
        );
        require(
            imported.is_err()
                && checksum(&preserved, 512 * 1024 * 1024)? == before
                && !paths.execution.exists()
                && !paths.consumer_execution.exists(),
            "OCI reader promoted failed diagnostics or changed retained native inputs",
        )
    })
}
