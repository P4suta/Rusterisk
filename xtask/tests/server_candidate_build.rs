#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::PathBuf, time::Duration};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{
        check_server_tar_gzip, CapturedServer, NativeServerBuildLimits, ServerCandidate,
        ServerPayload, ServerTarget,
    },
};

fn binary() -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    bytes
}

fn windows_binary() -> Vec<u8> {
    let mut bytes = vec![0; 256];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
    bytes[84..86].copy_from_slice(&112u16.to_le_bytes());
    bytes[86..88].copy_from_slice(&2u16.to_le_bytes());
    bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes[104..108].copy_from_slice(&4096u32.to_le_bytes());
    bytes[156..158].copy_from_slice(&3u16.to_le_bytes());
    bytes
}

#[test]
fn explicit_native_build_limits_reach_cargo_and_preserve_windows_candidate_cleanup(
) -> Result<(), SdkCandidateBuildError> {
    for timeout in [false, true] {
        using_project("server-explicit-limits", |root, parent| {
            write(
                root,
                "docs/server-distribution.md",
                b"Startup instructions.\n",
            )?;
            let capture = CapturedServer::read(root, ServerTarget::WindowsX86_64)?;
            let destination = parent.join("candidate");
            let limits = NativeServerBuildLimits::new(1, 1800)?;
            let mut execution = None;
            let mut phases = Vec::new();
            let result = capture.build_with_limits(&destination, limits, |command, budget| {
                let command = command.into_std();
                let phase = command
                    .get_args()
                    .next()
                    .and_then(|argument| argument.to_str())
                    .ok_or(ProcessError::InvalidOutput)?;
                phases.push(phase.to_owned());
                let source = command
                    .get_current_dir()
                    .ok_or(ProcessError::InvalidOutput)?;
                execution = source.parent().map(std::path::Path::to_owned);
                assert_ne!(source, root);
                assert_eq!(std::fs::read(source.join("config/http.conf"))?, b"fixture");
                let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
                assert_eq!(
                    environment
                        .get(std::ffi::OsStr::new("CARGO_BUILD_JOBS"))
                        .copied()
                        .flatten(),
                    Some(std::ffi::OsStr::new("1"))
                );
                match phase {
                    "fetch" => assert_eq!(budget, Duration::from_secs(180)),
                    "build" => {
                        assert_eq!(budget, Duration::from_secs(1800));
                        let arguments: Vec<_> = command.get_args().collect();
                        assert!(arguments
                            .windows(2)
                            .any(|pair| pair[0] == "--jobs" && pair[1] == "1"));
                        let target = PathBuf::from(
                            environment
                                .get(std::ffi::OsStr::new("CARGO_TARGET_DIR"))
                                .copied()
                                .flatten()
                                .ok_or(ProcessError::InvalidOutput)?,
                        );
                        if timeout {
                            let state = target.parent().ok_or(ProcessError::InvalidOutput)?;
                            std::fs::write(state.join("build.txt"), b"injected build timeout\n")?;
                            return Err(ProcessError::Timeout);
                        }
                        write(
                            &target,
                            "x86_64-pc-windows-msvc/release/rusterisk.exe",
                            &windows_binary(),
                        )?;
                    }
                    _ => return Err(ProcessError::InvalidOutput),
                }
                Ok(())
            });
            assert_eq!(phases, ["fetch", "build"]);
            assert!(!execution
                .ok_or_else(|| io::Error::other("execution directory missing"))?
                .exists());
            if timeout {
                let error = result
                    .err()
                    .ok_or_else(|| io::Error::other("timed out build passed"))?;
                assert!(error.to_string().contains("deadline exceeded"));
                assert_eq!(
                    std::fs::read(destination.join("build.txt"))?,
                    b"injected build timeout\n"
                );
                assert!(!destination.join("SHA256SUMS").exists());
            } else {
                result?;
                let candidate = ServerCandidate::read(&destination, ServerTarget::WindowsX86_64)?;
                assert_eq!(candidate.source_digest(), capture.source_digest());
                assert!(candidate
                    .files()
                    .any(|(path, bytes)| path == "bin/rusterisk.exe" && bytes == windows_binary()));
            }
            capture.check(root)?;
            Ok(())
        })?;
    }
    Ok(())
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
                let command = command.into_std();
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
            let command = command.into_std();
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
            let command = command.into_std();
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
