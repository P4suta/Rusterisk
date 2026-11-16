use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

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
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([
                "distribution",
                "sdk",
                "verify",
                "--candidate",
                "missing-candidate",
            ])
            .current_dir(&root)
            .env("PATH", "")
            .output()?;
        Ok::<_, std::io::Error>((output, root.join(".cache").exists()))
    })();
    let cleanup = std::fs::remove_dir_all(&root);
    let (output, cache_created) = match (operation, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(operation), Err(cleanup)) => {
            return Err(std::io::Error::other(format!(
                "{operation}; CLI fixture cleanup failed: {cleanup}"
            )))
        }
    };
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("usage:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!cache_created);
    Ok(())
}
