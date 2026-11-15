use std::process::Command;

#[test]
fn verification_build_dry_run_requires_no_daemon_and_keeps_resource_limits(
) -> Result<(), std::io::Error> {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["verification-build", "--dry-run"])
        .env("PATH", "")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let source = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "rusterisk-verification-preview",
        "memory=2g",
        "--pids-limit=256",
        "budget=900s",
        "containers/input-verification",
        "buildx rm --force",
    ] {
        assert!(source.contains(expected), "{expected}");
    }
    Ok(())
}

#[test]
fn verification_startup_failure_still_attempts_owned_builder_cleanup() -> Result<(), std::io::Error>
{
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["verification-build", "--run"])
        .env("PATH", "")
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("containers/input-verification"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup failed"));
    Ok(())
}
