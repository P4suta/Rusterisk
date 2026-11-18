use super::fixture::{invoke, require, using};
use std::{fs, io, path::Path, process::Output};
use xtask::release_check::{CapturedReleaseSource, RequiredGate};

const INPUTS: &[u8] = include_bytes!("inputs.toml");

fn text(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Release CLI fixture path was not UTF-8"))
}

fn rejected(output: &Output, reason: &str) -> io::Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    require(
        output.status.code() == Some(1) && output.stdout.is_empty(),
        &format!("Rejected release verification changed its status or emitted success: {stderr}"),
    )?;
    require(
        stderr.contains(reason) && !stderr.contains("unavailable") && !stderr.contains("panicked"),
        &format!("Connected release verification lost rejection reason {reason:?}: {stderr}"),
    )
}

#[test]
fn every_connected_release_gate_rejects_missing_source_before_starting_checks() -> io::Result<()> {
    using(|fixture| {
        let inputs = fixture.write("inputs.toml", INPUTS)?;
        let selected = text(&inputs)?;
        for gate in RequiredGate::ALL {
            let gate = gate.to_string();
            let output = invoke(
                &["release-check", "run", &gate, "--inputs", selected],
                fixture.root(),
            )?;
            rejected(&output, "release-check captured source:")?;
        }
        let output = invoke(
            &["release-check", "verify", "--inputs", selected],
            fixture.root(),
        )?;
        rejected(&output, "release-check captured source:")?;
        require(
            !fixture.path("missing-source").exists()
                && !fixture.path("candidates").exists()
                && !fixture.path("artifacts").exists(),
            "Missing release source created candidate or artifact state",
        )?;
        require(
            fs::read(inputs)? == INPUTS,
            "Missing release source changed selected inputs",
        )
    })
}

#[test]
fn connected_verification_rejects_missing_retained_artifacts_before_fresh_checks() -> io::Result<()>
{
    using(|fixture| {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| io::Error::other("Workspace directory is missing"))?;
        let captured = CapturedReleaseSource::read(project).map_err(io::Error::other)?;
        let source = fixture.path("missing-source");
        captured
            .write(&source, &fixture.path("captured-state"))
            .map_err(io::Error::other)?;
        let inputs = fixture.write("inputs.toml", INPUTS)?;
        let output = invoke(
            &["release-check", "verify", "--inputs", text(&inputs)?],
            fixture.root(),
        )?;
        rejected(&output, "release-check reference:")?;
        captured.check(&source).map_err(io::Error::other)?;
        require(
            !fixture.path("candidates").exists() && !fixture.path("artifacts").exists(),
            "Missing retained artifact created candidate or publication state",
        )?;
        require(
            fs::read(inputs)? == INPUTS,
            "Artifact reader changed selected inputs",
        )
    })
}
