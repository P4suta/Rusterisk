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

#[test]
fn interop_dry_run_reports_local_ports_deadlines_and_cleanup() -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["interop", "--dry-run"])
        .current_dir(workspace)
        .env("PATH", "")
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("127.0.0.1::8088"));
    assert!(stdout.contains(
        "budget=300s cargo test --locked --package rusterisk-client --test reference resource_queries_and_app_subscriptions_match_the_reference -- --ignored --exact --show-output"
    ));
    assert_eq!(stdout.matches("budget=300s cargo test ").count(), 1);
    assert!(stdout.contains("budget=180s cargo test --locked --package rusterisk-server --test sip_call blind_transfer::reference_blind_transfer_matches_native_received_audio -- --ignored --exact --show-output"));
    assert!(stdout.contains("budget=180s cargo test --locked --package rusterisk-server --test sip_call attended_transfer::reference_attended_transfer_matches_native_received_audio -- --ignored --exact --show-output"), "{stdout}");
    assert_eq!(stdout.matches("budget=180s cargo test ").count(), 2);
    assert!(stdout.contains("docker rm --force rusterisk-interop-preview"));
    Ok(())
}

#[test]
fn interop_dry_run_requires_a_fresh_server_and_the_shared_sipp_comparison(
) -> Result<(), std::io::Error> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["interop", "--dry-run"])
        .current_dir(workspace)
        .env("PATH", "")
        .output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "budget=300s cargo build --locked --package rusterisk-server --bin rusterisk --target-dir target --jobs 1"
    ), "{stdout}");
    assert!(
        stdout.contains("SIPp: authenticated registration and removal with Asterisk and Rusterisk"),
        "{stdout}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn packaged_browser_comparison_requires_a_linux_candidate_before_docker() -> std::io::Result<()> {
    let image = format!("sha256:{}", "a".repeat(64));
    let missing = std::env::temp_dir().join(format!(
        "rusterisk-unavailable-browser-candidate-{}",
        std::process::id()
    ));
    for (target, reason) in [
        (
            "x86_64-pc-windows-msvc",
            "requires a Linux server candidate",
        ),
        (
            "aarch64-unknown-linux-gnu",
            "packaged browser server candidate",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([
                "interop",
                "browser",
                "--dry-run",
                "--image",
                &image,
                "--reference-image",
                &image,
                "--server-target",
                target,
                "--server-candidate",
            ])
            .arg(&missing)
            .env("PATH", "")
            .output()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(reason), "{stderr}");
    }
    Ok(())
}
