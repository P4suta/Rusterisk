use super::{
    require, using_fixture, verify_with, Fixture, NativeGate, NativeOutcome, NativePlan, Port,
};
use crate::reference::ReferenceImage;
use crate::release_check::{
    CandidateCheckError, CandidateChecks, CapturedReleaseSource, FastOutcome, RequiredGate,
    RunnerOutcome, SourceInputs,
};
use std::{fs, io};

use crate::release_check::aggregate::profile_fixture;

fn reference_image() -> io::Result<ReferenceImage> {
    ReferenceImage::parse_inspection(&format!("sha256:{}\n", "a".repeat(64)))
        .map_err(io::Error::other)
}

fn completed_fast<'a>(
    captured: &'a CapturedReleaseSource,
    fixture: &mut Fixture,
) -> io::Result<FastOutcome<'a>> {
    let working = fixture.working.clone();
    let outcome = verify_with(
        captured,
        NativePlan::new(NativeGate::Fast),
        &working,
        Port { fixture, captured },
    )
    .map_err(io::Error::other)?;
    let NativeOutcome::Fast(outcome) = outcome else {
        return Err(io::Error::other(
            "The fast fixture returned another native outcome",
        ));
    };
    Ok(outcome)
}

#[test]
fn missing_native_outcomes_cannot_complete_the_required_inventory() -> io::Result<()> {
    using_fixture("check", |captured, _fixture| {
        let reference = reference_image()?;
        let checks = CandidateChecks::new(captured, profile_fixture::profile(&reference)?)
            .map_err(io::Error::other)?;
        require(
            matches!(
                checks.finish(),
                Err(CandidateCheckError::Missing(RequiredGate::Fast))
            ),
            "An empty candidate check collection completed its required gates",
        )
    })
}

#[test]
fn a_duplicate_native_outcome_cannot_replace_or_remove_an_accepted_gate() -> io::Result<()> {
    using_fixture("check", |captured, fixture| {
        let outcome = completed_fast(captured, fixture)?;
        let reference = reference_image()?;
        let mut checks = CandidateChecks::new(captured, profile_fixture::profile(&reference)?)
            .map_err(io::Error::other)?;
        checks
            .include(RunnerOutcome::Fast(&outcome))
            .map_err(io::Error::other)?;
        require(
            matches!(
                checks.include(RunnerOutcome::Fast(&outcome)),
                Err(CandidateCheckError::Duplicate(RequiredGate::Fast))
            ),
            "A duplicate native outcome replaced an accepted gate",
        )?;
        require(
            matches!(
                checks.finish(),
                Err(CandidateCheckError::Missing(RequiredGate::Security))
            ),
            "Duplicate rejection removed the original fast outcome or completed missing gates",
        )
    })
}

#[test]
fn changed_source_bytes_cannot_admit_a_completed_native_outcome() -> io::Result<()> {
    using_fixture("check", |captured, fixture| {
        let working = fixture.working.clone();
        let outcome = completed_fast(captured, fixture)?;
        captured
            .write(&working, &working.with_file_name("aggregate-state"))
            .map_err(io::Error::other)?;
        fs::write(working.join("Cargo.lock"), b"changed aggregate source\n")?;
        let changed = CapturedReleaseSource::read(&working).map_err(io::Error::other)?;
        let reference = reference_image()?;
        let mut checks = CandidateChecks::new(&changed, profile_fixture::profile(&reference)?)
            .map_err(io::Error::other)?;
        require(
            checks.include(RunnerOutcome::Fast(&outcome)).is_err(),
            "Changed source admitted a completed native outcome",
        )?;
        require(
            matches!(
                checks.finish(),
                Err(CandidateCheckError::Missing(RequiredGate::Fast))
            ),
            "Source rejection filled the fast outcome slot",
        )
    })
}

#[test]
fn an_added_release_input_cannot_reuse_an_older_native_input_subset() -> io::Result<()> {
    using_fixture("check", |captured, fixture| {
        let working = fixture.working.clone();
        let outcome = completed_fast(captured, fixture)?;
        captured
            .write(&working, &working.with_file_name("aggregate-state"))
            .map_err(io::Error::other)?;
        fs::write(
            working.join("api/aggregate-added.txt"),
            b"additional release input\n",
        )?;
        let changed = CapturedReleaseSource::read(&working).map_err(io::Error::other)?;
        let old = SourceInputs::new(outcome.inputs()).map_err(io::Error::other)?;
        let current = changed.inputs().map_err(io::Error::other)?;
        require(
            current.bind(&old).is_ok(),
            "The added-input counterexample changed existing input bytes",
        )?;
        let reference = reference_image()?;
        let mut checks = CandidateChecks::new(&changed, profile_fixture::profile(&reference)?)
            .map_err(io::Error::other)?;
        require(
            checks.include(RunnerOutcome::Fast(&outcome)).is_err(),
            "A source subset counted as the complete native input set",
        )
    })
}
