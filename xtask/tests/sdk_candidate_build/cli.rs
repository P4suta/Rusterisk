use super::fixture::{using_project, write};
use std::process::Command;
use xtask::distribution::SdkCandidateBuildError;

#[test]
fn sdk_distribution_plan_is_read_only_and_requires_no_external_tool(
) -> Result<(), SdkCandidateBuildError> {
    using_project("plan-cli", |root, _| {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["distribution", "sdk", "plan"])
            .current_dir(root)
            .env("PATH", "")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("budget=180s cargo fetch --locked"), "{text}");
        assert!(text.contains("budget=900s cargo package --locked --offline --jobs 2 --package rusterisk-core --package rusterisk-api --package rusterisk-client --target-dir"), "{text}");
        assert!(!text.contains("--no-verify"));
        assert!(!root.join(".cache").exists());
        Ok(())
    })
}

#[test]
fn sdk_distribution_build_rejects_invalid_inputs_before_creating_artifacts(
) -> Result<(), SdkCandidateBuildError> {
    using_project("build-cli", |root, _| {
        write(root, "docs/rust-sdk.md", b"")?;
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["distribution", "sdk", "build"])
            .current_dir(root)
            .env("PATH", "")
            .output()?;
        assert!(!output.status.success());
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("docs/rust-sdk.md"), "{text}");
        assert!(!root.join(".cache").exists());
        Ok(())
    })
}

#[test]
fn sdk_distribution_recipes_delegate_to_the_typed_project_entrypoint() {
    let recipes = include_str!("../../../justfile");
    for operation in ["plan", "build"] {
        assert!(recipes.contains(&format!("distribution-sdk-{operation}:\n    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk {operation}")));
    }
}
