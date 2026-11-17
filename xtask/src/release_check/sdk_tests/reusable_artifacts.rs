#![cfg(unix)]

use super::{
    captured_fixture, failure, require, update_source_checksum, using_fixture, Fault, Fixture,
};
use crate::{
    distribution::{SdkConsumerAction, SdkConsumerPhase, SdkConsumerScope, SdkContents},
    release_check::{CapturedReleaseSource, SdkOutcome, SdkRunError},
};
use std::{fs, io};

#[path = "reusable_artifacts/missing_tools.rs"]
mod missing_tools;

fn using(
    check: impl FnOnce(&CapturedReleaseSource, &mut Fixture) -> io::Result<()>,
) -> io::Result<()> {
    using_fixture(|_, fixture| {
        fs::write(
            fixture.candidate.join("source/rust-toolchain.toml"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../rust-toolchain.toml"
            )),
        )?;
        update_source_checksum(&fixture.candidate)?;
        let captured = CapturedReleaseSource::read(&fixture.candidate.join("source"))
            .map_err(io::Error::other)?;
        fixture.contents = SdkContents::read(&fixture.candidate).map_err(io::Error::other)?;
        check(&captured, fixture)
    })
}

#[test]
fn changed_or_missing_release_inputs_cannot_publish_sdk_artifacts() -> io::Result<()> {
    for (path, expected) in [
        ("config/http.conf", "differs"),
        ("xtask/src/unbound.rs", "missing"),
    ] {
        using(|captured, fixture| {
            captured_fixture::write(
                &fixture.candidate.join("source"),
                path,
                b"different SDK candidate input\n",
            )?;
            update_source_checksum(&fixture.candidate)?;
            SdkContents::read(&fixture.candidate).map_err(io::Error::other)?;
            let result: Result<SdkOutcome<'_>, SdkRunError> =
                captured.verify_sdk_artifact(&fixture.candidate, &fixture.artifacts);
            let error = result.err().ok_or_else(|| {
                io::Error::other("Unbound SDK inputs published a reusable artifact")
            })?;
            require(
                error.to_string().contains(expected) && !fixture.artifacts.exists(),
                "SDK artifact production lost its source binding or published rejected inputs",
            )
        })?;
    }
    Ok(())
}

#[test]
fn invalid_candidate_archives_cannot_publish_sdk_artifacts() -> io::Result<()> {
    using(|captured, fixture| {
        let archive = fixture.candidate.join("rusterisk-api-0.1.0.crate");
        fs::write(&archive, b"invalid SDK archive\n")?;
        let result: Result<SdkOutcome<'_>, SdkRunError> =
            captured.verify_sdk_artifact(&fixture.candidate, &fixture.artifacts);
        require(
            result.is_err()
                && !fixture.artifacts.exists()
                && fs::read(&archive)? == b"invalid SDK archive\n",
            "SDK artifact production accepted or changed an invalid candidate",
        )
    })
}

#[test]
fn sdk_artifact_production_preserves_an_existing_destination() -> io::Result<()> {
    using(|captured, fixture| {
        fs::create_dir(&fixture.artifacts)?;
        let marker = fixture.artifacts.join("foreign");
        fs::write(&marker, b"existing SDK artifact\n")?;
        let result: Result<SdkOutcome<'_>, SdkRunError> =
            captured.verify_sdk_artifact(&fixture.candidate, &fixture.artifacts);
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("SDK producer reused an existing artifact path"))?;
        require(
            error.to_string().contains("destination")
                && fs::read(&marker)? == b"existing SDK artifact\n",
            "SDK producer changed a destination outside its custody",
        )
    })
}

#[test]
fn missing_and_partial_sdk_artifacts_return_no_outcome() -> io::Result<()> {
    using(|captured, fixture| {
        let missing: Result<SdkOutcome<'_>, SdkRunError> =
            captured.read_sdk_artifact(&fixture.artifacts, &fixture.candidate);
        require(
            missing.is_err() && !fixture.artifacts.exists(),
            "Missing SDK artifacts returned an outcome or created a bundle",
        )?;
        fs::create_dir(&fixture.artifacts)?;
        let output = fixture.artifacts.join("core-msrv-run.stdout.txt");
        fs::write(&output, b"core candidate consumer passed\n")?;
        let partial: Result<SdkOutcome<'_>, SdkRunError> =
            captured.read_sdk_artifact(&fixture.artifacts, &fixture.candidate);
        require(
            partial.is_err() && fs::read(&output)? == b"core candidate consumer passed\n",
            "Partial SDK outputs returned an outcome or changed during rejection",
        )
    })
}

#[test]
fn failed_sdk_diagnostics_keep_both_causes_and_never_become_reusable_outcomes() -> io::Result<()> {
    using(|captured, fixture| {
        fixture.fault = Fault::PhaseAndFinish;
        let error = failure(captured, fixture)?;
        require(
            error.contains("fixture SDK phase failure")
                && error.contains("fixture SDK supervisor failure"),
            "SDK diagnostic preparation lost a primary or cleanup failure",
        )?;
        let diagnostic = fixture.artifacts.join("core-msrv-build.stderr.txt");
        let before = fs::read(&diagnostic)?;
        require(!before.is_empty(), "SDK phase diagnostic is empty")?;
        let result: Result<SdkOutcome<'_>, SdkRunError> =
            captured.read_sdk_artifact(&fixture.artifacts, &fixture.candidate);
        require(
            result.is_err() && fs::read(diagnostic)? == before && !fixture.execution.exists(),
            "SDK reader promoted failed diagnostics or changed their retained bytes",
        )
    })
}

#[test]
fn eight_phase_format_fixtures_without_native_toolchains_are_diagnostics() -> io::Result<()> {
    using(|captured, fixture| {
        fixture.fault = Fault::Finish;
        let error = failure(captured, fixture)?;
        require(
            error.contains("fixture SDK supervisor failure")
                && fixture.phases == SdkConsumerPhase::ALL,
            "SDK diagnostic fixture skipped a native-format phase or its cleanup failure",
        )?;
        for scope in SdkConsumerScope::ALL {
            require(
                fixture
                    .artifacts
                    .join("consumers")
                    .join(scope.label())
                    .join("Cargo.lock")
                    .is_file(),
                "SDK diagnostic fixture lost its checked consumer lockfile",
            )?;
            let phase = SdkConsumerPhase::new(scope, SdkConsumerAction::Run);
            require(
                !fs::read(
                    fixture
                        .artifacts
                        .join(format!("{}.stdout.txt", phase.label())),
                )?
                .is_empty(),
                "SDK diagnostic fixture lost its consumer output",
            )?;
        }
        let result: Result<SdkOutcome<'_>, SdkRunError> =
            captured.read_sdk_artifact(&fixture.artifacts, &fixture.candidate);
        require(
            result.is_err() && !fixture.execution.exists(),
            "SDK format fixtures replaced actual MSRV and development toolchain execution",
        )
    })
}
