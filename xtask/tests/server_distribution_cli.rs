#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, process::Command};
use xtask::{distribution::SdkCandidateBuildError, server_distribution::ServerTarget};

#[test]
fn server_distribution_plans_require_one_target_and_never_invoke_external_tools(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-plan-cli", |root, _| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        for target in ServerTarget::ALL {
            let result = Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args([
                    "distribution",
                    "server",
                    "plan",
                    "--target",
                    target.triple(),
                ])
                .current_dir(root)
                .env("PATH", "")
                .output()?;
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output = std::str::from_utf8(&result.stdout).map_err(io::Error::other)?;
            assert!(output.contains("server source="));
            assert!(output.contains(target.triple()));
            assert!(output.contains("phase=fetch budget=180s cargo fetch --locked --target"));
            assert!(
                output.contains("phase=build budget=900s cargo build --locked --offline --release")
            );
            assert!(!root.join(".cache").exists());
        }
        for arguments in [
            ["distribution", "server", "plan", "--target", "unsupported"],
            [
                "distribution",
                "server",
                "plan",
                "--destination",
                "aarch64-apple-darwin",
            ],
        ] {
            assert!(!Command::new(env!("CARGO_BIN_EXE_xtask"))
                .args(arguments)
                .current_dir(root)
                .env("PATH", "")
                .status()?
                .success());
        }
        Ok(())
    })
}

#[test]
fn server_build_startup_failures_keep_candidate_inputs_and_release_execution_state(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-build-cli-failure", |root, _| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args([
                "distribution",
                "server",
                "build",
                "--target",
                "aarch64-apple-darwin",
            ])
            .current_dir(root)
            .env("PATH", "")
            .output()?;
        assert!(!output.status.success());
        let parent = root.join(".cache/distributions/server");
        assert!(parent.is_dir());
        let candidates = std::fs::read_dir(parent)?.collect::<io::Result<Vec<_>>>()?;
        assert_eq!(candidates.len(), 1);
        let candidate = candidates[0].path();
        assert!(candidate.join("SOURCE.sha256").is_file());
        assert!(candidate
            .join("source/docs/server-distribution.md")
            .is_file());
        assert!(candidate.join("fetch.txt").is_file());
        assert!(!candidate.join("SHA256SUMS").exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("server candidate created"));
        Ok(())
    })
}
