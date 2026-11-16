use super::fixture::{using_project, write};
use std::path::{Path, PathBuf};
use xtask::distribution::{CapturedSdk, SdkCandidateBuildError};

#[test]
fn sdk_capture_validates_inputs_and_remains_independent_of_later_checkout_edits(
) -> Result<(), SdkCandidateBuildError> {
    using_project("capture", |root, _| {
        let capture = CapturedSdk::read(root)?;
        assert_eq!(capture.version(), "0.1.0");
        assert_eq!(capture.source_digest().len(), 64);
        write(root, "crates/client/src/lib.rs", b"live edit")?;
        assert_eq!(capture.version(), "0.1.0");
        write(root, "crates/client/LICENSE-MIT", b"changed license")?;
        assert!(CapturedSdk::read(root).is_err());
        Ok(())
    })
}

#[test]
fn sdk_build_preserves_the_primary_failure_and_logs_and_removes_the_execution_copy(
) -> Result<(), SdkCandidateBuildError> {
    using_project("failure", |root, parent| {
        let capture = CapturedSdk::read(root)?;
        let candidate = parent.join("candidate");
        let mut execution_source: Option<PathBuf> = None;
        let result = capture.build_with(&candidate, |command, _| {
            execution_source = command.get_current_dir().map(Path::to_owned);
            Err(xtask::process::ProcessError::Timeout)
        });
        assert!(result.is_err());
        assert!(result
            .err()
            .is_some_and(|error| error.to_string().contains("deadline exceeded")));
        let source =
            execution_source.ok_or_else(|| std::io::Error::other("SDK fetch was not invoked"))?;
        assert_ne!(source, root);
        assert!(!source.exists());
        assert!(candidate.join("fetch.txt").is_file());
        assert_eq!(
            std::fs::read(candidate.join("source/crates/client/src/lib.rs"))?,
            b"fixture"
        );
        assert_eq!(
            std::fs::read_to_string(candidate.join("SOURCE.sha256"))?.trim_end(),
            capture.source_digest()
        );
        Ok(())
    })
}

#[test]
fn sdk_build_reports_execution_source_changes_with_the_primary_process_error(
) -> Result<(), SdkCandidateBuildError> {
    using_project("changed", |root, parent| {
        let capture = CapturedSdk::read(root)?;
        let result = capture.build_with(&parent.join("candidate"), |command, _| {
            let source = command
                .get_current_dir()
                .ok_or(xtask::process::ProcessError::InvalidOutput)?;
            std::fs::write(source.join("crates/client/src/lib.rs"), b"changed")?;
            Err(xtask::process::ProcessError::Timeout)
        });
        assert!(matches!(
            &result,
            Err(SdkCandidateBuildError::SourceChanged { .. })
        ));
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("changed source was accepted"))?
            .to_string();
        assert!(error.contains("deadline exceeded"), "{error}");
        assert!(error.contains("SDK source check failed"), "{error}");
        Ok(())
    })
}

#[test]
fn sdk_build_does_not_replace_an_existing_candidate() -> Result<(), SdkCandidateBuildError> {
    using_project("existing", |root, parent| {
        let capture = CapturedSdk::read(root)?;
        let destination = parent.join("candidate");
        std::fs::create_dir(&destination)?;
        std::fs::write(destination.join("preserved"), b"original")?;
        let mut invoked = false;
        assert!(capture
            .build_with(&destination, |_, _| {
                invoked = true;
                Ok(())
            })
            .is_err());
        assert!(!invoked);
        assert_eq!(std::fs::read(destination.join("preserved"))?, b"original");
        Ok(())
    })
}

#[test]
fn sdk_build_requires_three_candidate_archives_after_a_successful_process(
) -> Result<(), SdkCandidateBuildError> {
    using_project("missing", |root, parent| {
        let capture = CapturedSdk::read(root)?;
        let candidate = parent.join("candidate");
        let mut phases = 0;
        let result = capture.build_with(&candidate, |_, _| {
            phases += 1;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(phases, 2);
        assert!(candidate.join("package.txt").is_file());
        assert!(!candidate.join("SHA256SUMS").exists());
        Ok(())
    })
}
