#[path = "sdk_content/fixture.rs"]
mod archive_fixture;
#[path = "sdk_contents/fixture.rs"]
mod candidate_fixture;
#[path = "sdk_candidate_build/fixture.rs"]
mod captured_fixture;
#[path = "sdk_http_metadata/fixture.rs"]
pub mod http_fixture;
#[path = "sdk_consumer_metadata/fixture.rs"]
pub mod metadata_fixture;

mod sdk_http_run {
    mod checks;
    mod fixture;
}
