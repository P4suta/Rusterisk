#[path = "windows_job_accounting/source.rs"]
mod fixtures;

#[test]
fn the_workspace_accounting_source_has_only_its_registered_query() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let path = "crates/windows-job-accounting/src/lib.rs";
    let source = std::fs::read_to_string(root.join(path))?;
    let exceptions: xtask::Exceptions = toml::from_str(&std::fs::read_to_string(
        root.join("api/policy-exceptions.toml"),
    )?)
    .map_err(std::io::Error::other)?;
    let scoped: Vec<_> = exceptions
        .exceptions
        .into_iter()
        .filter(|exception| exception.path == path)
        .collect();
    let findings = xtask::inspect_windows_job_accounting_source(&source, &scoped)
        .map_err(std::io::Error::other)?;
    assert!(findings.is_empty(), "{findings:?}");
    Ok(())
}
