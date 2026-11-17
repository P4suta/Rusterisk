use super::{native_target, require, using};
use crate::{
    release_check::{
        candidate_run::{checksum, Loaded},
        ServerOutcome, ServerRunError,
    },
    server_distribution::{ServerExecutionMode, ServerTarget},
};
use std::{fs, io};

#[test]
fn changed_server_inputs_cannot_publish_reusable_native_artifacts() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        paths.change_server_source()?;
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.verify_server_artifact(
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Changed server inputs published a native artifact"))?;
        require(
            error.to_string().contains("differs") && !paths.artifacts.exists(),
            "Reusable native producer lost source binding or published rejected inputs",
        )
    })
}

#[test]
fn same_version_sdk_changes_cannot_publish_reusable_native_artifacts() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        paths.change_sdk_source()?;
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.verify_server_artifact(
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Changed SDK inputs published a native artifact"))?;
        require(
            error.to_string().contains("differs") && !paths.artifacts.exists(),
            "Reusable native producer accepted SDK version equality without source binding",
        )
    })
}

#[test]
fn a_foreign_native_target_cannot_publish_reusable_artifacts() -> io::Result<()> {
    using(ServerTarget::WindowsX86_64, |captured, paths| {
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.verify_server_artifact(
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("A foreign host published a native target artifact"))?;
        require(
            error.to_string().contains("requested host policy") && !paths.artifacts.exists(),
            "Reusable artifact publication bypassed native execution policy",
        )
    })
}

#[test]
fn the_public_native_producer_preserves_an_existing_destination() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        fs::create_dir(&paths.artifacts)?;
        fs::write(paths.artifacts.join("foreign"), b"existing artifact\n")?;
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.verify_server_artifact(
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
            &paths.artifacts,
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Native producer reused an existing artifact path"))?;
        require(
            error.to_string().contains("destination")
                && fs::read(paths.artifacts.join("foreign"))? == b"existing artifact\n",
            "Native producer changed a destination outside its custody",
        )
    })
}

#[test]
fn preserved_diagnostics_cannot_be_imported_as_a_native_server_outcome() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        let loaded = Loaded::read(captured, &paths.server, paths.target, &paths.sdk)
            .map_err(io::Error::other)?;
        fs::create_dir(&paths.artifacts)?;
        loaded
            .preserve(captured, &paths.artifacts)
            .map_err(io::Error::other)?;
        fs::create_dir(paths.artifacts.join("server"))?;
        fs::write(
            paths.artifacts.join("server/version.stdout.txt"),
            b"rusterisk 0.1.0\n",
        )?;
        fs::write(
            paths.artifacts.join("server/server.stdout.txt"),
            b"native diagnostic output\n",
        )?;
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.read_server_artifact(
            &paths.artifacts,
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
        );
        require(
            result.is_err(),
            "Legacy native diagnostics were promoted to a completed server outcome",
        )?;
        let archive = paths
            .artifacts
            .join("server-candidate")
            .join(loaded.server.name().file_name());
        require(
            checksum(&archive, 272 * 1024 * 1024)? == loaded.server.checksum(),
            "Rejected diagnostic import changed the preserved server candidate",
        )?;
        captured
            .check(&paths.artifacts.join("release-source"))
            .map_err(io::Error::other)
    })
}

#[test]
fn partial_native_outputs_cannot_be_imported_as_a_server_outcome() -> io::Result<()> {
    using(native_target()?, |captured, paths| {
        fs::create_dir(&paths.artifacts)?;
        fs::create_dir(paths.artifacts.join("server"))?;
        let version = paths.artifacts.join("server/version.stdout.txt");
        fs::write(&version, b"rusterisk 0.1.0\n")?;
        let result: Result<ServerOutcome<'_>, ServerRunError> = captured.read_server_artifact(
            &paths.artifacts,
            &paths.server,
            paths.target,
            &paths.sdk,
            ServerExecutionMode::Native,
        );
        require(
            result.is_err() && fs::read(&version)? == b"rusterisk 0.1.0\n",
            "Partial native outputs returned an outcome or changed during rejection",
        )
    })
}
