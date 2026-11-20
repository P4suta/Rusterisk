//! Exercises retained lifecycle decisions with the private proof model.

use super::{execute_with, prepare, ProofPort};
use crate::{
    core_proofs::{
        artifact_fixture::{using_fixture, Fixture, IMAGE},
        validate_results, CapturedCoreProofs, ProofError, HARNESSES,
    },
    process::ProcessError,
    release_check::{CapturedReleaseSource, SourceInputs},
};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
    time::Duration,
};

#[derive(Clone, Copy)]
enum AtFinish {
    Preserve,
    Reject,
    ChangeProjection,
    ChangeRelease,
}

#[derive(Default)]
struct Observed {
    finishes: usize,
    native: Vec<u8>,
}

struct Port<'a> {
    fixture: &'a mut Fixture,
    destination: &'a Path,
    observed: &'a mut Observed,
    at_finish: AtFinish,
}

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn change(path: &Path) -> Result<(), ProcessError> {
    let mut file = fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

impl ProofPort for Port<'_> {
    fn run(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        self.fixture.perform_work(command, budget)
    }

    fn run_cleanup(&mut self, command: &mut Command, budget: Duration) -> Result<(), ProcessError> {
        self.fixture.perform_cleanup(command, budget)
    }

    fn finish(self) -> Result<(), ProcessError> {
        self.observed.finishes += 1;
        require(
            !self.destination.try_exists()?,
            "Modeled proofs published before supervisor finish",
        )?;
        let working = self.fixture.working()?;
        let native = fs::read(working.join("results/kani.json"))?;
        validate_results(&native).map_err(|error| {
            io::Error::other(format!("Proof model native precondition failed: {error}"))
        })?;
        self.observed.native = native;
        match self.at_finish {
            AtFinish::Preserve => Ok(()),
            AtFinish::Reject => Err(ProcessError::InvalidOutput),
            AtFinish::ChangeProjection => change(&working.join("source/crates/core/src/lib.rs")),
            AtFinish::ChangeRelease => {
                change(&working.join("release-source/xtask/src/core_proofs.rs"))
            }
        }
    }
}

fn release_source() -> io::Result<CapturedReleaseSource> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Proof model project directory is missing"))?;
    let captured = CapturedReleaseSource::read(root).map_err(io::Error::other)?;
    let core = CapturedCoreProofs::read(root).map_err(io::Error::other)?;
    let inputs = SourceInputs::new(core.inputs()).map_err(io::Error::other)?;
    captured
        .inputs()
        .and_then(|release| release.bind(&inputs))
        .map_err(io::Error::other)?;
    Ok(captured)
}

fn cleaned(fixture: &Fixture, observed: &Observed) -> io::Result<()> {
    require(
        fixture.runs == 1 && fixture.cleanups == 0 && observed.finishes == 1,
        "Proof model did not execute and finish its single maintained command",
    )?;
    validate_results(&observed.native).map_err(io::Error::other)?;
    require(
        !fixture.working()?.try_exists()?,
        "Retained proof execution left its owned working directory",
    )?;
    for entry in fs::read_dir(&fixture.root)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name
            .to_str()
            .ok_or_else(|| io::Error::other("Proof model entry is not UTF-8"))?;
        require(
            !text.starts_with("proof-artifacts-"),
            "Retained proof publication left a staging directory",
        )?;
    }
    Ok(())
}

#[test]
fn modeled_retained_proofs_publish_after_finish_and_preserve_native_bytes() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let destination = fixture.root.join("published");
        let prepared = prepare(&captured, IMAGE, &destination).map_err(io::Error::other)?;
        let mut observed = Observed::default();
        let outcome = execute_with(
            prepared,
            Port {
                fixture,
                destination: &destination,
                observed: &mut observed,
                at_finish: AtFinish::Preserve,
            },
        )
        .map_err(io::Error::other)?;
        cleaned(fixture, &observed)?;
        require(
            outcome.image() == IMAGE && outcome.harnesses() == HARNESSES,
            "Modeled proof outcome lost its image or declared harnesses",
        )?;
        let metadata: toml::Value =
            toml::from_str(&fs::read_to_string(destination.join("native-check.toml"))?)
                .map_err(io::Error::other)?;
        require(
            metadata.get("solver").and_then(toml::Value::as_str) == Some("kissat")
                && metadata.get("scratch_mount").and_then(toml::Value::as_str)
                    == Some("type=volume,target=/tmp,volume-nocopy"),
            "Retained proof metadata omitted its solver or scratch boundary",
        )?;
        let expected = SourceInputs::new(outcome.inputs())
            .map_err(io::Error::other)?
            .key();
        require(
            fs::read(destination.join("native/results/kani.json"))? == observed.native,
            "Retained proof publication changed its native JSON bytes",
        )?;
        captured
            .check(&destination.join("native/release-source"))
            .map_err(io::Error::other)?;
        let retained = captured
            .read_core_proofs_artifact(&destination, IMAGE)
            .map_err(io::Error::other)?;
        require(
            SourceInputs::new(retained.inputs())
                .map_err(io::Error::other)?
                .key()
                == expected
                && retained.image() == IMAGE
                && retained.harnesses() == HARNESSES,
            "Modeled proof reader changed the retained source, image, or harnesses",
        )?;
        let metadata_path = destination.join("native-check.toml");
        let original = fs::read(&metadata_path)?;
        for (field, value) in [
            ("solver", "cadical"),
            ("scratch_mount", "type=tmpfs,target=/tmp"),
            ("scratch_mount", "type=bind,source=/foreign,target=/tmp"),
        ] {
            let mut changed = metadata.clone();
            changed[field] = value.into();
            fs::write(
                &metadata_path,
                toml::to_string(&changed).map_err(io::Error::other)?,
            )?;
            require(
                captured
                    .read_core_proofs_artifact(&destination, IMAGE)
                    .is_err(),
                "Retained proof reader accepted a different execution profile",
            )?;
        }
        fs::write(metadata_path, original)?;
        Ok(())
    })
}

#[test]
fn modeled_finish_failure_prevents_artifact_publication() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let destination = fixture.root.join("published");
        let prepared = prepare(&captured, IMAGE, &destination).map_err(io::Error::other)?;
        let mut observed = Observed::default();
        let error = execute_with(
            prepared,
            Port {
                fixture,
                destination: &destination,
                observed: &mut observed,
                at_finish: AtFinish::Reject,
            },
        )
        .err()
        .ok_or_else(|| io::Error::other("Failed model finish produced an outcome"))?;
        cleaned(fixture, &observed)?;
        require(
            !destination.try_exists()?
                && error
                    .to_string()
                    .contains(&ProcessError::InvalidOutput.to_string()),
            "Failed model finish published an artifact or lost its primary error",
        )
    })
}

#[test]
fn modeled_source_changes_at_finish_prevent_artifact_publication() -> io::Result<()> {
    for at_finish in [AtFinish::ChangeProjection, AtFinish::ChangeRelease] {
        using_fixture(|fixture| {
            let captured = release_source()?;
            let destination = fixture.root.join("published");
            let prepared = prepare(&captured, IMAGE, &destination).map_err(io::Error::other)?;
            let mut observed = Observed::default();
            let error = execute_with(
                prepared,
                Port {
                    fixture,
                    destination: &destination,
                    observed: &mut observed,
                    at_finish,
                },
            )
            .err()
            .ok_or_else(|| io::Error::other("Changed model source produced an outcome"))?;
            cleaned(fixture, &observed)?;
            require(
                !destination.try_exists()?
                    && error
                        .to_string()
                        .contains(&ProofError::SourceChanged.to_string()),
                "Changed model source published an artifact or lost its source rejection",
            )
        })?;
    }
    Ok(())
}
