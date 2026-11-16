use std::process::Command;

const IMAGE: &str = "registry.example/core-assurance@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn assurance_dry_runs_bind_actual_tests_and_require_no_daemon() -> Result<(), std::io::Error> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    for task in ["core-miri", "core-mutation"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([task, "--dry-run", "--image", IMAGE])
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
            "21 integration targets",
            "60 tests",
            "--cpus=2",
            "--memory=2g",
            "budget=900s",
            "docker rm --force",
            IMAGE,
        ] {
            assert!(stdout.contains(expected), "{expected}");
        }
        if task == "core-mutation" {
            for expected in [
                "2 source-bound field mutations",
                "terminal-state-build",
                "connection-claim-tests",
                "budget=60s",
            ] {
                assert!(stdout.contains(expected), "{expected}");
            }
        }
        let rejected = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([task, "--dry-run", "--image", "latest"])
            .current_dir(project)
            .env("PATH", "")
            .output()?;
        assert!(!rejected.status.success());
    }
    Ok(())
}

#[test]
fn startup_failure_removes_all_owned_source_and_result_files() -> Result<(), std::io::Error> {
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| std::io::Error::other("workspace missing"))?;
    let directory =
        std::env::temp_dir().join(format!("rusterisk-assurance-cli-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let operation = (|| {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["core-miri", "--run", "--image", IMAGE])
            .current_dir(project)
            .env("PATH", "")
            .env("TMPDIR", &directory)
            .env("TMP", &directory)
            .env("TEMP", &directory)
            .output()?;
        if output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains("phase=miri-setup")
            || !String::from_utf8_lossy(&output.stderr).contains("assurance cleanup failed")
        {
            return Err(std::io::Error::other(
                "startup failure did not reach the Miri setup phase",
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
