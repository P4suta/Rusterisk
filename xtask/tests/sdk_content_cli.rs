#[path = "sdk_content/fixture.rs"]
mod archive_fixture;
#[path = "sdk_contents/fixture.rs"]
mod candidate_fixture;
#[path = "sdk_candidate_build/fixture.rs"]
mod captured_fixture;

use std::process::Command;

#[test]
fn the_content_command_reads_only_selected_candidate_bytes_without_external_tools(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "cli",
        |_| Ok(()),
        |candidate| {
            let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args(["distribution", "sdk", "check-content", "--candidate"])
                .arg(candidate)
                .current_dir(candidate)
                .env("PATH", "")
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let digest = std::fs::read_to_string(candidate.join("SOURCE.sha256"))?;
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!(
                    "distribution: SDK candidate contents verified source={} version=0.1.0\n",
                    digest.trim_end()
                )
            );
            assert!(output.stderr.is_empty());
            Ok(())
        },
    )
}

#[test]
fn stale_candidate_content_fails_without_success_output_or_mutating_files(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "cli-stale",
        |_| Ok(()),
        |candidate| {
            let path = candidate.join("SOURCE.sha256");
            std::fs::write(&path, b"stale\n")?;
            let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args(["distribution", "sdk", "check-content", "--candidate"])
                .arg(candidate)
                .env("PATH", "")
                .output()?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("SOURCE.sha256"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read(path)?, b"stale\n");
            Ok(())
        },
    )
}
