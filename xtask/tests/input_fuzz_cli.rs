use std::process::Command;
use xtask::{input_fuzz::Target, release_check::SourceInputs};

const IMAGE: &str = "registry.example/verification@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn fuzz_dry_runs_bind_real_inputs_without_a_daemon() -> Result<(), std::io::Error> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input-fuzz", "all", "--dry-run", "--image", IMAGE])
        .current_dir(project)
        .env("PATH", "")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        format!("{} targets", Target::ALL.len()),
        format!(
            "snapshot <= {} MiB",
            SourceInputs::MAX_TOTAL_BYTES / (1024 * 1024)
        ),
        format!("project files <= {}", SourceInputs::MAX_FILES - 512),
    ] {
        assert!(stdout.contains(&expected), "{expected}");
    }
    for expected in [
        "vendor files <= 512",
        "phase=fetch",
        "phase=build-settings",
        "phase=run-media_packets",
        "budget=900s",
        "budget=90s",
        "--network=none",
        "-runs=100000",
        "/project/crates/api",
        "docker rm --force",
    ] {
        assert!(stdout.contains(expected), "{expected}");
    }
    for arguments in [
        ["input-fuzz", "unknown", "--dry-run", "--image", IMAGE].as_slice(),
        ["input-fuzz", "settings", "--dry-run", "--image", "latest"].as_slice(),
        [
            "input-fuzz",
            "settings",
            "--dry-run",
            "--image",
            IMAGE,
            "extra",
        ]
        .as_slice(),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(arguments)
            .current_dir(project)
            .env("PATH", "")
            .output()?;
        assert!(!output.status.success());
    }
    Ok(())
}

#[test]
fn fuzz_startup_failure_removes_owned_source_and_state() -> Result<(), std::io::Error> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let directory = std::env::temp_dir().join(format!("rusterisk-fuzz-cli-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let operation = (|| {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["input-fuzz", "settings", "--run", "--image", IMAGE])
            .current_dir(project)
            .env("PATH", "")
            .env("TMPDIR", &directory)
            .env("TMP", &directory)
            .env("TEMP", &directory)
            .output()?;
        if output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains("phase=rust-version")
            || !String::from_utf8_lossy(&output.stderr).contains("input fuzz cleanup failed")
        {
            return Err(std::io::Error::other(
                "startup did not attempt the bounded Rust version check",
            ));
        }
        if std::fs::read_dir(&directory)?.next().is_some() {
            return Err(std::io::Error::other("startup failure leaked owned files"));
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir(&directory);
    operation.and(cleanup)
}
