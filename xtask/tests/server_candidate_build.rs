#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::PathBuf};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{check_server_tar_gzip, CapturedServer, ServerPayload, ServerTarget},
};

fn binary() -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    bytes
}

#[test]
fn server_builds_archive_only_the_executable_produced_from_captured_source(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-build", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        let destination = parent.join("candidate");
        let mut execution: Option<PathBuf> = None;
        let mut calls = 0;
        capture
            .build_with(&destination, |command, budget| {
                calls += 1;
                let source = command
                    .get_current_dir()
                    .ok_or(ProcessError::InvalidOutput)?;
                assert_ne!(source, root);
                assert_eq!(std::fs::read(source.join("config/http.conf"))?, b"fixture");
                assert!(budget.as_secs() <= 900);
                execution = source.parent().map(std::path::Path::to_owned);
                if command
                    .get_args()
                    .next()
                    .is_some_and(|argument| argument == "build")
                {
                    let state = command
                        .get_envs()
                        .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                        .and_then(|(_, value)| value)
                        .ok_or(ProcessError::InvalidOutput)?;
                    write(
                        &PathBuf::from(state),
                        "aarch64-apple-darwin/release/rusterisk",
                        &binary(),
                    )?;
                }
                Ok(())
            })
            .map_err(io::Error::other)?;
        assert_eq!(calls, 2);
        assert!(!execution
            .ok_or_else(|| io::Error::other("execution directory missing"))?
            .exists());
        let payload = ServerPayload::capture(ServerTarget::MacOsArm64, binary(), |path| {
            std::fs::read(destination.join("source").join(path))
        })
        .map_err(io::Error::other)?;
        let archive = std::fs::read(destination.join(payload.name().file_name()))?;
        check_server_tar_gzip(&payload, &archive)?;
        assert!(destination.join("fetch.txt").is_file());
        assert!(destination.join("build.txt").is_file());
        assert!(destination.join("SHA256SUMS").is_file());
        assert_eq!(
            std::fs::read_to_string(destination.join("SOURCE.sha256"))?.trim(),
            capture.source_digest()
        );
        assert!(capture.build_with(&destination, |_, _| Ok(())).is_err());
        Ok(())
    })
}

#[test]
fn failed_builds_preserve_logs_and_the_primary_error_and_remove_the_execution_copy(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-build-failure", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        let destination = parent.join("candidate");
        let mut execution: Option<PathBuf> = None;
        let result = capture.build_with(&destination, |command, _| {
            let source = command
                .get_current_dir()
                .ok_or(ProcessError::InvalidOutput)?;
            execution = source.parent().map(std::path::Path::to_owned);
            std::fs::write(source.join("docs/server-distribution.md"), b"changed")?;
            Err(ProcessError::Timeout)
        });
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("changed failed build passed"))?
            .to_string();
        assert!(error.contains("deadline exceeded"), "{error}");
        assert!(error.contains("source"), "{error}");
        assert!(destination.join("fetch.txt").is_file());
        assert!(!destination.join("SHA256SUMS").exists());
        assert!(!execution
            .ok_or_else(|| io::Error::other("execution directory missing"))?
            .exists());
        Ok(())
    })
}

#[test]
fn successful_process_status_cannot_replace_the_required_target_executable(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-build-no-output", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        let destination = parent.join("candidate");
        assert!(capture.build_with(&destination, |_, _| Ok(())).is_err());
        assert!(!destination.join("SHA256SUMS").exists());
        Ok(())
    })
}

#[test]
fn build_output_for_another_target_cannot_be_packaged_as_the_selected_target(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-build-wrong-target", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::MacOsArm64).map_err(io::Error::other)?;
        let destination = parent.join("candidate");
        let result = capture.build_with(&destination, |command, _| {
            if command
                .get_args()
                .next()
                .is_some_and(|argument| argument == "build")
            {
                let target = command
                    .get_envs()
                    .find(|(name, _)| *name == "CARGO_TARGET_DIR")
                    .and_then(|(_, value)| value)
                    .ok_or(ProcessError::InvalidOutput)?;
                let mut bytes = binary();
                bytes[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
                write(
                    &PathBuf::from(target),
                    "aarch64-apple-darwin/release/rusterisk",
                    &bytes,
                )?;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert!(!destination.join("SHA256SUMS").exists());
        assert!(destination.join("build.txt").is_file());
        Ok(())
    })
}
