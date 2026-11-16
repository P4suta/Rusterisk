#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{
    io,
    path::{Path, PathBuf},
};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{
        export_server_candidate_oci_with, export_server_oci_with, CapturedServer,
        ServerBuildCommandMode, ServerCandidate, ServerTarget,
    },
};

fn candidate(root: &Path, parent: &Path) -> io::Result<ServerCandidate> {
    write(
        root,
        "docs/server-distribution.md",
        b"Startup instructions.\n",
    )?;
    let capture = CapturedServer::read(root, ServerTarget::LinuxArm64).map_err(io::Error::other)?;
    let directory = parent.join("candidate");
    capture.build_with(&directory, |command, _| {
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
            let mut binary = vec![0; 64];
            binary[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
            binary[16..18].copy_from_slice(&2_u16.to_le_bytes());
            binary[18..20].copy_from_slice(&183_u16.to_le_bytes());
            binary[20..24].copy_from_slice(&1_u32.to_le_bytes());
            binary[24..32].copy_from_slice(&4096_u64.to_le_bytes());
            binary[52..54].copy_from_slice(&64_u16.to_le_bytes());
            write(
                &PathBuf::from(state),
                "aarch64-unknown-linux-gnu/release/rusterisk",
                &binary,
            )?;
        }
        Ok(())
    })?;
    ServerCandidate::read(&directory, ServerTarget::LinuxArm64)
}

#[test]
fn image_export_interruptions_attempt_builder_cleanup_and_keep_both_failures(
) -> Result<(), SdkCandidateBuildError> {
    using_project("oci-export-interrupted", |root, parent| {
        let candidate = candidate(root, parent)?;
        let parent = std::fs::canonicalize(parent)?;
        let execution = parent.join("execution");
        let artifacts = parent.join("artifacts");
        let mut work = 0;
        let mut cleanup = 0;
        let result = export_server_oci_with(
            &candidate,
            &execution,
            &artifacts,
            |command, budget, mode| {
                assert_eq!(command.get_program(), "docker");
                assert!(budget.as_secs() <= 300);
                match mode {
                    ServerBuildCommandMode::Work => {
                        work += 1;
                        assert!(execution.join("payload/bin/rusterisk").is_file());
                        if work == 2 {
                            Err(ProcessError::Interrupted)
                        } else {
                            Ok(())
                        }
                    }
                    ServerBuildCommandMode::Cleanup => {
                        cleanup += 1;
                        Err(io::Error::other("injected builder cleanup failure").into())
                    }
                }
            },
        );
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("interrupted export passed"))?
            .to_string();
        assert!(error.contains("interrupted"));
        assert!(error.contains("injected builder cleanup failure"));
        assert_eq!((work, cleanup), (2, 1));
        assert!(!execution.exists());
        assert!(artifacts.join("bootstrap.txt").is_file());
        assert!(artifacts.join("cleanup.txt").is_file());
        assert!(!artifacts.join("SHA256SUMS").exists());
        Ok(())
    })
}

#[test]
fn successful_status_cannot_replace_exported_bytes_or_unchanged_contexts(
) -> Result<(), SdkCandidateBuildError> {
    using_project("oci-export-no-bytes", |root, parent| {
        let candidate = candidate(root, parent)?;
        let parent = std::fs::canonicalize(parent)?;
        for mutate in [false, true] {
            let execution = parent.join(format!("execution-{mutate}"));
            let artifacts = parent.join(format!("artifacts-{mutate}"));
            let mut work = 0;
            let mut cleanup = 0;
            let result =
                export_server_oci_with(&candidate, &execution, &artifacts, |_, _, mode| {
                    match mode {
                        ServerBuildCommandMode::Work => {
                            work += 1;
                            if mutate {
                                std::fs::write(execution.join("Dockerfile"), b"changed")?;
                            }
                        }
                        ServerBuildCommandMode::Cleanup => cleanup += 1,
                    }
                    Ok(())
                });
            assert!(result.is_err());
            assert_eq!(work, if mutate { 1 } else { 4 });
            assert_eq!(cleanup, 1);
            assert!(!execution.exists());
            assert!(!artifacts.join("SHA256SUMS").exists());
        }
        Ok(())
    })
}

#[test]
fn candidate_exports_reject_nonempty_archives_with_invalid_image_contents(
) -> Result<(), SdkCandidateBuildError> {
    using_project("oci-export-invalid-content", |root, parent| {
        let _candidate = candidate(root, parent)?;
        let mut cleanup = 0;
        let result = export_server_candidate_oci_with(
            root,
            &parent.join("candidate"),
            ServerTarget::LinuxArm64,
            |command, _, mode| {
                match mode {
                    ServerBuildCommandMode::Cleanup => cleanup += 1,
                    ServerBuildCommandMode::Work => {
                        for argument in command.get_args() {
                            if let Some(output) = argument
                                .to_str()
                                .and_then(|arg| arg.strip_prefix("type=oci,dest="))
                            {
                                let destination = output
                                    .split(',')
                                    .next()
                                    .ok_or(ProcessError::InvalidOutput)?;
                                std::fs::write(destination, b"invalid OCI archive")?;
                            }
                        }
                    }
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(cleanup, 1);
        Ok(())
    })
}
