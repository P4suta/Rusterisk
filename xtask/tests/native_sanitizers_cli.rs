use std::process::Command;
#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;

const IMAGE: &str = "registry.example/verification@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn native_dry_runs_show_instrumentation_and_boundaries_without_a_daemon() -> std::io::Result<()> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["native-sanitizers", "--dry-run", "--image", IMAGE])
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
        "source=",
        "phase=clang-version",
        "phase=reject-probe-address",
        "phase=reject-probe-undefined",
        "phase=inspect-opus",
        "phase=inspect-sqlite",
        "phase=inspect-aws-lc",
        "phase=run-media",
        "phase=run-storage",
        "phase=run-tls",
        "-fsanitize=address,undefined",
        "--network=none",
        "--cpus=1",
        "budget=900s",
        "docker rm --force",
    ] {
        assert!(stdout.contains(expected), "{expected}");
    }
    for arguments in [
        ["native-sanitizers", "--dry-run", "--image", "latest"].as_slice(),
        ["native-sanitizers", "--unknown", "--image", IMAGE].as_slice(),
        ["native-sanitizers", "--dry-run", "--image", IMAGE, "extra"].as_slice(),
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

#[cfg(unix)]
#[test]
fn native_startup_failure_preserves_results_and_removes_only_execution_state(
) -> Result<(), xtask::distribution::SdkCandidateBuildError> {
    fixture::using_project("native-cli", |project, parent| {
        let temporary = parent.join("execution");
        std::fs::create_dir(&temporary)?;
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["native-sanitizers", "--run", "--image", IMAGE])
            .current_dir(project)
            .env("PATH", "")
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .output()?;
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("native phase=clang-version"), "{stdout}");
        assert!(!stdout.contains("boundary tests passed"), "{stdout}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("native container cleanup failed"),
            "{stderr}"
        );
        assert!(std::fs::read_dir(temporary)?.next().is_none());
        let mut candidates = std::fs::read_dir(project.join(".cache/native-sanitizers"))?;
        let candidate = candidates
            .next()
            .ok_or_else(|| std::io::Error::other("native result missing"))??
            .path();
        assert!(candidates.next().is_none());
        for relative in [
            "SOURCE.sha256",
            "clang-version.txt",
            "source/crates/core/src/lib.rs",
        ] {
            assert!(candidate.join(relative).is_file(), "{relative}");
        }
        Ok(())
    })
}
