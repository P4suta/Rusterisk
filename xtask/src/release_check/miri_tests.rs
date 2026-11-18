#![cfg(unix)]

use super::read_bound_miri;
use crate::{
    core_assurance::Phase,
    core_miri::{CoreMiriError, CoreMiriOutcome},
    process::{CommandSupervisor, ProcessError},
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    time::Duration,
};

#[path = "miri_tests/fixture.rs"]
mod fixture;
use fixture::{require, using, IMAGE};

const SOURCE: &str = "RUSTERISK_MIRI_ARTIFACT_MISSING_TOOLS_SOURCE";

#[test]
fn retained_miri_capture_binds_full_release_inputs_and_source_derived_inventory() -> io::Result<()>
{
    using(|captured, paths| {
        let miri = read_bound_miri(captured, &paths.source).map_err(io::Error::other)?;
        let projection = SourceInputs::new(miri.inputs()).map_err(io::Error::other)?;
        let bound = captured
            .inputs()
            .map_err(io::Error::other)?
            .bind(&projection)
            .map_err(io::Error::other)?;
        require(
            bound == projection.key() && !miri.targets().is_empty() && !miri.tests().is_empty(),
            "Miri capture lost its bound inputs or authoritative native test inventory",
        )?;
        require(
            !paths.artifacts.exists(),
            "Pure Miri capture created an execution artifact",
        )
    })
}

#[test]
fn core_noncore_and_lock_changes_fail_full_release_validation_before_miri_capture() -> io::Result<()>
{
    for path in [
        "crates/core/src/lib.rs",
        "clippy.toml",
        "crates/core/assurance/Cargo.lock",
    ] {
        using(|captured, paths| {
            fs::write(
                paths.source.join(path),
                b"different retained release input\n",
            )?;
            let error = read_bound_miri(captured, &paths.source)
                .err()
                .ok_or_else(|| io::Error::other("Changed release inputs reached Miri capture"))?;
            let diagnostic = error.to_string();
            require(
                diagnostic.contains("release") && diagnostic.contains("changed"),
                "Miri retained inputs bypassed full release validation or lost its diagnostic",
            )?;
            require(
                !paths.artifacts.exists(),
                "Miri source rejection created execution state",
            )
        })?;
    }
    Ok(())
}

#[test]
fn additional_core_source_fails_full_release_validation() -> io::Result<()> {
    using(|captured, paths| {
        fs::write(
            paths.source.join("crates/core/src/retained_extra.rs"),
            b"pub const EXTRA: bool = true;\n",
        )?;
        let error = read_bound_miri(captured, &paths.source)
            .err()
            .ok_or_else(|| io::Error::other("Additional retained source reached Miri capture"))?;
        require(
            error.to_string().contains("release") && !paths.artifacts.exists(),
            "Miri capture accepted extra release source or lost its validation stage",
        )
    })
}

#[test]
fn miri_artifact_production_rejects_mutable_or_malformed_image_selections() -> io::Result<()> {
    using(|captured, paths| {
        for image in ["", "rusterisk-verification:latest", "sha256:invalid"] {
            let result: Result<CoreMiriOutcome, CoreMiriError> =
                captured.verify_core_miri_artifact(image, &paths.artifacts);
            require(
                matches!(result, Err(CoreMiriError::Plan)) && !paths.artifacts.exists(),
                "Miri artifact production accepted an invalid image or created execution state",
            )?;
        }
        Ok(())
    })
}

#[test]
fn miri_artifact_production_preserves_an_existing_destination() -> io::Result<()> {
    using(|captured, paths| {
        read_bound_miri(captured, &paths.source).map_err(io::Error::other)?;
        fs::create_dir(&paths.artifacts)?;
        let marker = paths.artifacts.join("foreign");
        fs::write(&marker, b"existing Miri artifact\n")?;
        let result: Result<CoreMiriOutcome, CoreMiriError> =
            captured.verify_core_miri_artifact(IMAGE, &paths.artifacts);
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("Miri producer replaced an existing destination"))?;
        require(
            error.to_string().contains("destination")
                && fs::read(marker)? == b"existing Miri artifact\n",
            "Miri producer changed a destination outside its custody",
        )
    })
}

#[test]
fn missing_partial_and_legacy_miri_native_outputs_return_no_outcome() -> io::Result<()> {
    using(|captured, paths| {
        let missing: Result<CoreMiriOutcome, CoreMiriError> =
            captured.read_core_miri_artifact(&paths.artifacts, IMAGE);
        require(
            missing.is_err() && !paths.artifacts.exists(),
            "Missing Miri artifacts returned an outcome or created a bundle",
        )?;
        fs::create_dir(&paths.artifacts)?;
        let inventory = paths.artifacts.join("miri-inventory.txt");
        let prefix = b"{\"type\":\"suite\",\"event\":\"discovery\"}\n";
        fs::write(&inventory, prefix)?;
        for extra in [None, Some("miri-setup.txt"), Some("miri-tests.txt")] {
            if let Some(name) = extra {
                fs::write(paths.artifacts.join(name), b"")?;
            }
            let result: Result<CoreMiriOutcome, CoreMiriError> =
                captured.read_core_miri_artifact(&paths.artifacts, IMAGE);
            require(
                result.is_err() && fs::read(&inventory)? == prefix,
                "Miri reader promoted native diagnostics or changed rejected input bytes",
            )?;
        }
        Ok(())
    })
}

fn missing_native_tool(error: &CoreMiriError) -> bool {
    match error {
        CoreMiriError::Process(ProcessError::Io(error))
        | CoreMiriError::Phase {
            phase: Phase::MiriSetup,
            source: ProcessError::Io(error),
        } => error.kind() == io::ErrorKind::NotFound,
        CoreMiriError::Io(_)
        | CoreMiriError::Assurance(_)
        | CoreMiriError::Process(_)
        | CoreMiriError::Phase { .. }
        | CoreMiriError::Changed
        | CoreMiriError::Plan
        | CoreMiriError::FollowUp { .. } => false,
    }
}

#[test]
fn missing_tools_worker() -> io::Result<()> {
    let Some(source) = std::env::var_os(SOURCE) else {
        return Ok(());
    };
    let source = PathBuf::from(source);
    let captured = CapturedReleaseSource::read(&source).map_err(io::Error::other)?;
    read_bound_miri(&captured, &source).map_err(io::Error::other)?;
    let plan = crate::core_miri::plan(IMAGE).map_err(io::Error::other)?;
    require(
        plan.step(Phase::MiriSetup)
            .arguments
            .iter()
            .any(|argument| argument == IMAGE),
        "Miri missing-tool fixture has an invalid native plan",
    )?;
    let published = source.with_file_name("miri-artifact");
    require(
        !published.exists(),
        "Miri fixture destination already exists",
    )?;
    let result: Result<CoreMiriOutcome, CoreMiriError> =
        captured.verify_core_miri_artifact(IMAGE, &published);
    let error = result
        .err()
        .ok_or_else(|| io::Error::other("Missing native tools produced a Miri artifact"))?;
    require(
        missing_native_tool(&error) && !published.exists(),
        "Miri tool failure was masked by a preflight error or published an artifact",
    )?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(source.with_file_name("missing-tools-worker-completed"))?
        .write_all(b"Miri native tool failure observed\n")
}

#[test]
fn missing_native_tools_cannot_publish_miri_artifacts() -> io::Result<()> {
    using(|captured, paths| {
        read_bound_miri(captured, &paths.source).map_err(io::Error::other)?;
        let (_, module) = module_path!()
            .split_once("::")
            .ok_or_else(|| io::Error::other("Miri worker module lacks its crate prefix"))?;
        let worker = format!("{module}::missing_tools_worker");
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", &worker, "--nocapture", "--test-threads=1"])
            .env("PATH", "")
            .env(SOURCE, &paths.source);
        let mut supervisor = CommandSupervisor::new().map_err(io::Error::other)?;
        let operation = supervisor.run(&mut command, Duration::from_secs(5));
        let cleanup = supervisor.finish();
        match (operation, cleanup) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(io::Error::other(error)),
            (Err(operation), Err(cleanup)) => {
                return Err(io::Error::other(format!(
                    "{operation}; Miri artifact fixture cleanup failed: {cleanup}"
                )))
            }
        }
        require(
            !paths.artifacts.exists()
                && fs::read(
                    paths
                        .source
                        .with_file_name("missing-tools-worker-completed"),
                )? == b"Miri native tool failure observed\n",
            "Miri tool discovery published an artifact or skipped its isolated worker",
        )
    })
}
