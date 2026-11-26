#[path = "release_publication/fixture.rs"]
mod fixture;
use fixture::{plan, SOURCE};
use std::{fs, io};

#[test]
fn a_partial_candidate_cannot_start_publication() -> io::Result<()> {
    let empty = tempfile::tempdir()?;
    fs::write(empty.path().join("SHA256SUMS"), "")?;
    let output = plan(empty.path(), SOURCE)?;
    assert!(!output.status.success());
    let detail = String::from_utf8_lossy(&output.stderr);
    assert!(detail.contains("asset inventory"), "{detail}");
    Ok(())
}

#[test]
fn plan_binds_all_downloads_and_the_embedded_cli_source() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let files = fixture::files(SOURCE)?;
    fixture::write(root.path(), &files)?;
    let valid = plan(root.path(), SOURCE)?;
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(String::from_utf8_lossy(&valid.stdout).contains("20 assets"));
    assert!(!plan(root.path(), fixture::OTHER)?.status.success());
    fs::write(
        root.path()
            .join("sbom/rusterisk-0.2.0-aarch64-apple-darwin.cdx.xml"),
        "changed",
    )?;
    assert!(!plan(root.path(), SOURCE)?.status.success());
    Ok(())
}
