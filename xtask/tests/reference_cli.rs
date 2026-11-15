use std::process::Command;

#[test]
fn reference_dry_run_needs_no_daemon_and_reports_the_complete_resource_scope(
) -> Result<(), std::io::Error> {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["reference-build", "--dry-run"])
        .env("PATH", "")
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("memory=2g,memory-swap=2g"));
    assert!(stdout.contains("cpu-quota=200000,cpu-period=100000"));
    assert!(stdout.contains("--pids-limit=256"));
    assert!(stdout.contains("budget=1800s"));
    assert!(stdout.contains("docker buildx rm --force rusterisk-reference-preview"));
    let invalid = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["reference-build", "--invalid"])
        .env("PATH", "")
        .output()?;
    assert!(!invalid.status.success());
    Ok(())
}
