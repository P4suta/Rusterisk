use super::{require, using_fixture, verify_reference_with, Port};
use crate::{
    reference::ReferenceImage,
    release_check::{CandidateChecks, RunnerOutcome},
};
use std::io;

use crate::release_check::aggregate::profile_fixture;

#[test]
fn a_completed_reference_outcome_cannot_change_the_selected_immutable_image() -> io::Result<()> {
    using_fixture(|captured, image, fixture| {
        let working = fixture.working.clone();
        let outcome = verify_reference_with(
            captured,
            image,
            &working,
            Port {
                fixture,
                captured,
                image,
            },
        )
        .map_err(io::Error::other)?;
        let mut matching = CandidateChecks::new(captured, profile_fixture::profile(image)?)
            .map_err(io::Error::other)?;
        matching
            .include(RunnerOutcome::Reference(&outcome))
            .map_err(io::Error::other)?;
        let other = ReferenceImage::parse_inspection(&format!("sha256:{}\n", "b".repeat(64)))
            .map_err(io::Error::other)?;
        let mut changed = CandidateChecks::new(captured, profile_fixture::profile(&other)?)
            .map_err(io::Error::other)?;
        require(
            changed.include(RunnerOutcome::Reference(&outcome)).is_err(),
            "A completed reference outcome changed its immutable image profile",
        )
    })
}
