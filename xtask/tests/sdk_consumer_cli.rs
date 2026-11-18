use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const OVERFLOWING_JOBS: &str = "100000000000000000000000000000000000000000000000000";
const INVALID_JOBS: &str = "SDK consumer jobs must be a positive integer";

#[test]
fn verify_command_validates_candidates_without_starting_cargo_or_creating_cache_state(
) -> Result<(), std::io::Error> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-sdk-verify-cli-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let operation = (|| {
        let mut outputs = Vec::with_capacity(7);
        for (scope, jobs) in [
            ("sdk", None),
            ("sdk", Some("1")),
            ("sdk", Some("0")),
            ("sdk", Some(OVERFLOWING_JOBS)),
            ("server", Some("1")),
            ("server", Some("0")),
            ("server", Some(OVERFLOWING_JOBS)),
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
            command
                .args(["distribution", scope, "verify", "--candidate", "missing"])
                .current_dir(&root)
                .env("PATH", "");
            if scope == "server" {
                command.args(["--sdk-candidate", "missing-sdk"]);
            }
            if let Some(jobs) = jobs {
                command.args(["--consumer-jobs", jobs]);
            }
            outputs.push((command.output()?, jobs));
        }
        Ok::<_, std::io::Error>((outputs, root.join(".cache").exists()))
    })();
    let cleanup = std::fs::remove_dir_all(&root);
    let (outputs, cache_created) = match (operation, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(cleanup)) => {
            return Err(std::io::Error::other(format!(
                "{operation}; CLI fixture cleanup failed: {cleanup}"
            )))
        }
    };
    for (output, jobs) in outputs {
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!error.contains("usage:"), "{error}");
        assert_eq!(
            error.contains(INVALID_JOBS),
            matches!(jobs, Some("0" | OVERFLOWING_JOBS)),
            "{error}"
        );
    }
    assert!(!cache_created);
    Ok(())
}
