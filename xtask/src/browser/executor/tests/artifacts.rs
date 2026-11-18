use super::fixture::using_fixture;
use crate::{
    browser::{BrowserError, BrowserOutcome, RunError},
    release_check::CapturedReleaseSource,
};
use std::{fs, io, path::Path};

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn release_source() -> io::Result<CapturedReleaseSource> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("Workspace directory is missing"))?;
    CapturedReleaseSource::read(root).map_err(io::Error::other)
}

#[test]
fn invalid_images_cannot_start_or_publish_retained_browser_checks() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        let result: Result<BrowserOutcome, RunError> =
            captured.verify_browser_artifact("not-a-digest", &published);
        require(
            matches!(result, Err(RunError::Plan(BrowserError::Plan))) && !published.exists(),
            "Invalid browser image selection created a reusable artifact",
        )
    })
}

#[test]
fn retained_browser_checks_preserve_existing_publication_destinations() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let published = fixture.root.join("published");
        fs::create_dir(&published)?;
        let marker = published.join("foreign");
        fs::write(&marker, b"preserve existing publication")?;
        let result: Result<BrowserOutcome, RunError> =
            captured.verify_browser_artifact(&fixture.image, &published);
        require(
            result.is_err() && fs::read(marker)? == b"preserve existing publication",
            "Retained browser checks changed an existing publication",
        )
    })
}

#[test]
fn missing_browser_artifacts_are_rejected_without_creating_reader_state() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let missing = fixture.root.join("missing");
        let result: Result<BrowserOutcome, RunError> =
            captured.read_browser_artifact(&missing, &fixture.image);
        require(
            result.is_err() && !missing.exists(),
            "Browser artifact reader accepted or created missing retained inputs",
        )
    })
}

#[test]
fn partial_browser_diagnostics_cannot_be_imported_as_successful_publications() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = release_source()?;
        let diagnostic = fixture.root.join(".cache/browser-runs/diagnostic");
        fs::create_dir_all(&diagnostic)?;
        let report = diagnostic.join("browser-results.json");
        let output = diagnostic.join("run.txt");
        fs::write(&report, b"{}\n")?;
        fs::write(&output, b"incomplete native diagnostics\n")?;
        let result: Result<BrowserOutcome, RunError> =
            captured.read_browser_artifact(&diagnostic, &fixture.image);
        require(
            result.is_err()
                && fs::read(report)? == b"{}\n"
                && fs::read(output)? == b"incomplete native diagnostics\n",
            "Browser artifact reader promoted or rewrote incomplete diagnostics",
        )?;
        let entries = fs::read_dir(&diagnostic)?.collect::<Result<Vec<_>, io::Error>>()?;
        require(
            entries.len() == 2,
            "Browser artifact reader added publication state to diagnostics",
        )
    })
}
