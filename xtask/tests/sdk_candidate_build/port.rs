use super::fixture::using_project;
use xtask::distribution::{build_sdk_with, SdkCandidateBuildError};

#[test]
fn sdk_candidate_build_uses_its_borrowed_command_port_and_keeps_failure_logs(
) -> Result<(), SdkCandidateBuildError> {
    using_project("port", |root, _| {
        let mut calls = 0;
        let result = build_sdk_with(root, |command, _| {
            calls += 1;
            assert_eq!(command.get_program(), "cargo");
            assert_ne!(command.get_current_dir(), Some(root));
            Err(xtask::process::ProcessError::Timeout)
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
        let mut candidates = std::fs::read_dir(root.join(".cache/distributions/sdk"))?;
        let candidate = candidates
            .next()
            .transpose()?
            .ok_or_else(|| std::io::Error::other("candidate log directory missing"))?;
        assert!(candidates.next().is_none());
        assert!(candidate.path().join("fetch.txt").is_file());
        assert!(candidate.path().join("SOURCE.sha256").is_file());
        assert!(!candidate.path().join("SHA256SUMS").exists());
        Ok(())
    })
}
