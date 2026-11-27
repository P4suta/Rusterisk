use std::{fs, io, path::Path};

#[test]
fn only_manual_default_branch_jobs_can_authenticate_or_publish() -> io::Result<()> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/publish-cli.yml");
    let workflow = fs::read_to_string(path)?;
    let (header, jobs) = workflow
        .split_once("\njobs:\n")
        .ok_or_else(|| io::Error::other("release workflow jobs"))?;
    assert!(header.contains("\non:\n  workflow_dispatch:"));
    assert!(!header.contains("push:") && !header.contains("pull_request"));
    assert!(header.contains("default: verify-auth"));
    assert!(!workflow.contains("secrets.") && !workflow.contains("--clobber"));
    let job = |name: &str| -> io::Result<&str> {
        let (_, tail) = jobs
            .split_once(&format!("  {name}:\n"))
            .ok_or_else(|| io::Error::other(name.to_owned()))?;
        let end = tail
            .match_indices("\n  ")
            .find(|(i, _)| {
                tail.as_bytes()
                    .get(i + 3)
                    .is_some_and(u8::is_ascii_alphabetic)
            })
            .map_or(tail.len(), |(i, _)| i);
        Ok(&tail[..end])
    };
    let probe = job("verify-auth")?;
    let build = job("build")?;
    let verify = job("candidate")?;
    let registry = job("registry")?;
    let page = job("release-page")?;
    assert!(probe.contains("environment: crates-io") && probe.contains("id-token: write"));
    assert!(
        probe.contains("inputs.operation == 'verify-auth'") && probe.contains("refs/heads/main")
    );
    assert!(!probe.contains("run:") && !probe.contains("contents: write"));
    assert!(
        build.contains("refs/heads/main")
            && build.contains("contents: read")
            && !build.contains("id-token:")
    );
    assert!(
        verify.contains("needs: build")
            && verify.contains("export-cli")
            && !verify.contains("id-token:")
    );
    assert!(
        registry.contains("environment: crates-io")
            && registry.contains("id-token: write")
            && registry.contains("publish-archive")
    );
    assert!(!registry.contains("contents: write"));
    assert!(
        page.contains("environment: github-release")
            && page.contains("contents: write")
            && page.contains("publish-page")
    );
    assert!(page.contains("registry]") && page.contains("inputs.operation == 'publish-release'"));
    assert!(!page.contains("id-token:") && !page.contains("crates-io-auth-action"));
    Ok(())
}
