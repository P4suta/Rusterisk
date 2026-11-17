#[path = "sdk_content/fixture.rs"]
mod archive_fixture;
#[path = "sdk_candidate_build/fixture.rs"]
mod captured_fixture;
mod sdk_contents {
    use super::{archive_fixture, captured_fixture};
    mod checks;
    mod fixture;
}
