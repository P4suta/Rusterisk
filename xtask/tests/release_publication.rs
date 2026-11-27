#[path = "release_publication/fixture.rs"]
mod fixture;
#[cfg(unix)]
#[path = "release_publication/providers.rs"]
mod providers;
#[path = "release_publication/workflow.rs"]
mod workflow;
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
fn the_cli_archive_accepts_multiple_mebibytes_and_rejects_more_than_sixteen() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let name = "rusterisk-cli-0.2.0.crate";
    let mut files = fixture::files(SOURCE)?;
    let readme = "a".repeat(2 * 1024 * 1024);
    let archive = fixture::crate_with_readme(SOURCE, &readme, flate2::Compression::none())?;
    assert!(archive.len() >= readme.len());
    files.insert(name.into(), archive);
    fixture::write(root.path(), &files)?;
    let valid = plan(root.path(), SOURCE)?;
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    fs::OpenOptions::new()
        .write(true)
        .open(root.path().join(name))?
        .set_len(16 * 1024 * 1024 + 1)?;
    let oversized = plan(root.path(), SOURCE)?;
    assert!(!oversized.status.success());
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("size limit"));
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
