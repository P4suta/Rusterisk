use std::{io, path::Path, process::Command};

fn invoke(arguments: &[&str]) -> io::Result<std::process::Output> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace missing"))?;
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(arguments)
        .current_dir(root)
        .env("PATH", "")
        .output()
}

#[test]
fn a_release_preview_lists_every_required_gate_without_external_programs() -> io::Result<()> {
    let output = invoke(&["release-check", "--dry-run"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("release-check plan: 17 required gates"));
    for gate in [
        "fast",
        "security",
        "reference",
        "browser",
        "core-proofs",
        "core-miri",
        "core-mutation",
        "input-fuzz",
        "native-sanitizers",
        "sdk",
        "server:x86_64-unknown-linux-gnu",
        "server:aarch64-unknown-linux-gnu",
        "server:aarch64-apple-darwin",
        "server:x86_64-apple-darwin",
        "server:x86_64-pc-windows-msvc",
        "oci:x86_64-unknown-linux-gnu",
        "oci:aarch64-unknown-linux-gnu",
    ] {
        let expected = format!("required: {gate}");
        assert_eq!(
            stdout.lines().filter(|line| *line == expected).count(),
            1,
            "{gate}: {stdout}"
        );
    }
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("required: "))
            .count(),
        17
    );
    assert!(!stdout.contains("passed"));
    Ok(())
}

#[test]
fn release_execution_without_runner_inputs_cannot_claim_completion() -> io::Result<()> {
    let output = invoke(&["release-check", "--run"])?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("release-check requires explicit runner inputs"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"));
    Ok(())
}
