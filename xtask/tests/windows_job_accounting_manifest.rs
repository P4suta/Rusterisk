#[path = "windows_job_accounting/manifest.rs"]
mod fixtures;

#[test]
fn the_workspace_accounting_crate_matches_its_closed_manifest() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let workspace = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let member = std::fs::read_to_string(root.join("crates/windows-job-accounting/Cargo.toml"))?;
    let findings = xtask::inspect_windows_job_accounting_manifest(&workspace, &member)
        .map_err(std::io::Error::other)?;
    assert!(findings.is_empty(), "{findings:?}");
    Ok(())
}
