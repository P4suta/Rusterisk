#[path = "sdk_candidate_build/fixture.rs"]
mod fixture;
use fixture::{using_project, write};
use std::{io, path::PathBuf};
use xtask::{
    distribution::SdkCandidateBuildError,
    process::ProcessError,
    server_distribution::{CapturedServer, ServerBuildCommandMode, ServerCandidate, ServerTarget},
};

const IMAGE: &str = "sha256:8ba30003af95ec74b9dff0aaadcb661bcc68d260a50b912c4ac3203d67c7afbb";

fn state(command: &std::process::Command) -> Result<PathBuf, ProcessError> {
    command
        .get_args()
        .filter_map(|argument| argument.to_str())
        .find_map(|argument| {
            argument
                .strip_prefix("type=bind,source=")?
                .strip_suffix(",target=/state")
                .map(PathBuf::from)
        })
        .ok_or(ProcessError::InvalidOutput)
}

fn binary() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&4096_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes
}

#[test]
fn captured_linux_builds_package_the_container_output_and_remove_owned_state(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-container-candidate", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::LinuxArm64).map_err(io::Error::other)?;
        let candidate = parent.join("candidate");
        let mut execution = None;
        let mut calls = 0;
        capture.build_in_container_with(&candidate, IMAGE, |command, budget, mode| {
            assert_eq!(mode, ServerBuildCommandMode::Work);
            assert_eq!(command.get_program(), "docker");
            assert!(budget.as_secs() <= 900);
            let state = state(command)?;
            assert!(state.join("cargo").is_dir());
            assert!(state.join("tmp").is_dir());
            execution = state.parent().map(PathBuf::from);
            if calls == 1 {
                write(
                    &state,
                    "target/aarch64-unknown-linux-gnu/release/rusterisk",
                    &binary(),
                )?;
            }
            calls += 1;
            Ok(())
        })?;
        assert_eq!(calls, 2);
        assert!(!execution
            .ok_or_else(|| io::Error::other("missing execution state"))?
            .exists());
        assert!(ServerCandidate::read(&candidate, ServerTarget::LinuxArm64).is_ok());
        assert!(std::fs::read_to_string(candidate.join("build.txt"))?.contains(IMAGE));
        Ok(())
    })
}

#[test]
fn interrupted_container_builds_attempt_cleanup_and_preserve_both_failures(
) -> Result<(), SdkCandidateBuildError> {
    using_project("server-container-interrupted", |root, parent| {
        write(
            root,
            "docs/server-distribution.md",
            b"Startup instructions.\n",
        )?;
        let capture =
            CapturedServer::read(root, ServerTarget::LinuxArm64).map_err(io::Error::other)?;
        let mut execution = None;
        let mut calls = 0;
        let candidate = parent.join("candidate");
        let result = capture.build_in_container_with(&candidate, IMAGE, |command, _, mode| {
            calls += 1;
            match mode {
                ServerBuildCommandMode::Work => {
                    execution = state(command)?.parent().map(PathBuf::from);
                    Err(ProcessError::Interrupted)
                }
                ServerBuildCommandMode::Cleanup => {
                    assert_eq!(command.get_args().next(), Some(std::ffi::OsStr::new("rm")));
                    Err(io::Error::other("injected container cleanup failure").into())
                }
            }
        });
        let error = result
            .err()
            .ok_or_else(|| io::Error::other("interrupted build passed"))?
            .to_string();
        assert!(error.contains("interrupted"));
        assert!(error.contains("injected container cleanup failure"));
        assert_eq!(calls, 2);
        assert!(!execution
            .ok_or_else(|| io::Error::other("missing execution state"))?
            .exists());
        assert!(candidate.join("fetch.txt").is_file());
        assert!(!candidate.join("SHA256SUMS").exists());
        Ok(())
    })
}
