use std::process::Command;

#[test]
fn proof_image_preview_uses_no_daemon_and_reports_owned_limits() -> Result<(), std::io::Error> {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["core-proofs-build", "--dry-run"])
        .env("PATH", "")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "rusterisk-core-proofs-preview",
        "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000",
        "--pids-limit=256",
        "budget=900s",
        "containers/core-proofs",
        "rusterisk-core-proofs:kani-0.68.0-1bf25ce3",
        "docker buildx rm --force rusterisk-core-proofs-preview",
    ] {
        assert!(source.contains(expected), "{expected}");
    }
    Ok(())
}

#[test]
fn proof_image_startup_failure_attempts_owned_builder_cleanup() -> Result<(), std::io::Error> {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["core-proofs-build", "--run"])
        .env("PATH", "")
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("containers/core-proofs"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup failed"));
    let invalid = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["core-proofs-build", "--invalid"])
        .env("PATH", "")
        .output()?;
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stdout).contains("buildx create"));
    Ok(())
}
