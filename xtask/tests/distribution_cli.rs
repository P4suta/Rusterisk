use std::{path::Path, process::Command};

#[test]
fn native_server_plan_reports_the_selected_build_limits() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let plan = |options: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([
                "distribution",
                "server",
                "plan",
                "--target",
                "x86_64-pc-windows-msvc",
            ])
            .args(options)
            .current_dir(&root)
            .env("PATH", "")
            .output()
    };
    for (options, jobs, timeout) in [
        (&[][..], "2", "900"),
        (
            &["--jobs", "1", "--build-timeout-seconds", "1800"][..],
            "1",
            "1800",
        ),
    ] {
        let output = plan(options)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("phase=fetch budget=180s cargo fetch --locked"));
        assert!(stdout.contains(&format!(
            "phase=build budget={timeout}s cargo build --locked"
        )));
        assert!(stdout.contains(&format!("--jobs {jobs} --target-dir")));
    }
    for (jobs, seconds) in [("0", "1800"), ("1", "0"), ("1", "65536")] {
        let output = plan(&["--jobs", jobs, "--build-timeout-seconds", seconds])?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    Ok(())
}

#[test]
fn sdk_input_check_runs_without_cargo_docker_or_registry_access() -> Result<(), std::io::Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("distribution-check")
        .current_dir(root)
        .env("PATH", "")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "distribution: SDK package inputs verified\n"
    );
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn fast_gate_rejects_missing_package_documentation_before_external_checks(
) -> Result<(), std::io::Error> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-distribution-cli-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let operation = (|| {
        std::fs::write(root.join("Cargo.toml"), "[workspace.package]\nversion = '0.1.0'\nlicense = 'MIT OR Apache-2.0'\nrepository = 'https://github.com/P4suta/Rusterisk'\n")?;
        std::fs::write(root.join("LICENSE-MIT"), "MIT fixture\n")?;
        std::fs::write(root.join("LICENSE-APACHE"), "Apache fixture\n")?;
        let run = |task| {
            Command::new(env!("CARGO_BIN_EXE_xtask"))
                .arg(task)
                .current_dir(&root)
                .env("PATH", "")
                .output()
        };
        Ok::<_, std::io::Error>([run("distribution-check")?, run("check")?])
    })();
    let cleanup = std::fs::remove_dir_all(root);
    let outputs = match (operation, cleanup) {
        (Ok(outputs), Ok(())) => outputs,
        (Err(operation), Err(cleanup)) => {
            return Err(std::io::Error::other(format!(
                "{operation}; fixture cleanup failed: {cleanup}"
            )))
        }
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
    };
    for output in outputs {
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("docs/rust-sdk.md"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    Ok(())
}
