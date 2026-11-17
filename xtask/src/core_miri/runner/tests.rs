use super::{finish, verify_with, CommandMode};
use crate::{
    core_assurance::CapturedAssurance, core_miri::CapturedCoreMiri, process::ProcessError,
    release_check::SourceInputs,
};
use std::io;
#[path = "tests/fixture.rs"]
mod fixture;
use fixture::{using_fixture, Fault, IMAGE};

#[test]
fn miri_uses_the_existing_source_manifest_and_test_inventory() -> io::Result<()> {
    using_fixture(|fixture| {
        let shared =
            CapturedAssurance::read(&fixture.root.join("crates/core")).map_err(io::Error::other)?;
        let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
        assert_eq!(captured.targets(), shared.targets());
        assert_eq!(captured.tests(), shared.tests());
        assert_eq!(captured.targets().len(), 22);
        assert_eq!(captured.tests().len(), 62);
        let plan = crate::core_miri::plan(IMAGE).map_err(io::Error::other)?;
        assert!(plan
            .step(fixture::PHASES[0])
            .arguments
            .iter()
            .any(|argument| argument == IMAGE));
        assert!(fixture.temporary.is_none());
        Ok(())
    })
}

#[test]
fn miri_completion_requires_native_discovery_execution_and_all_cleanup() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
        let key = SourceInputs::new(captured.inputs())
            .map_err(io::Error::other)?
            .key();
        fixture.inventory(captured.targets().len(), captured.tests());
        std::fs::write(
            fixture.root.join("crates/core/src/lib.rs"),
            b"concurrent edit",
        )?;
        let checked = verify_with(captured, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        let outcome = finish(checked, Ok(())).map_err(io::Error::other)?;
        assert_eq!(outcome.test_count(), 62);
        assert_eq!(outcome.image(), IMAGE);
        assert_eq!(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key(),
            key
        );
        assert!(fixture.phases == fixture::PHASES);
        assert!(crate::core_assurance::validate_miri_execution(
            &fixture.report(true)?,
            outcome.inventory()
        )
        .is_ok());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn miri_rejects_missing_native_tests_skips_and_truncated_execution() -> io::Result<()> {
    for fault in [Fault::MissingDiscovery, Fault::Skipped, Fault::Truncated] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(captured.targets().len(), captured.tests());
            let checked = verify_with(captured, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn miri_rejects_changed_original_working_and_normalized_inputs() -> io::Result<()> {
    for fault in [
        Fault::ChangedOriginal,
        Fault::ChangedWorking,
        Fault::ChangedManifest,
        Fault::AddedSource,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(captured.targets().len(), captured.tests());
            let checked = verify_with(captured, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn miri_capture_rejects_ignored_or_undeclared_source_tests() -> io::Result<()> {
    using_fixture(|fixture| {
        let source = fixture.root.join("crates/core/tests/transitions.rs");
        let original = std::fs::read_to_string(&source)?;
        std::fs::write(
            source,
            original.replacen("#[test]", "#[test]\n#[ignore]", 1),
        )?;
        assert!(CapturedCoreMiri::read(&fixture.root).is_err());
        Ok(())
    })?;
    using_fixture(|fixture| {
        std::fs::write(
            fixture.root.join("crates/core/tests/unlisted.rs"),
            b"#[test]\nfn unlisted() {}\n",
        )?;
        assert!(CapturedCoreMiri::read(&fixture.root).is_err());
        Ok(())
    })
}

#[test]
fn miri_interruption_and_supervisor_errors_never_create_an_outcome() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::Interrupted;
        let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(captured, IMAGE, |command, budget, mode| match mode {
            CommandMode::Work => fixture.perform(command, budget, mode),
            CommandMode::Cleanup => {
                fixture.cleanups += 1;
                Err(ProcessError::InvalidOutput)
            }
        });
        let error = finish(checked, Err(ProcessError::Interrupted))
            .err()
            .ok_or_else(|| io::Error::other("interruption completed"))?
            .to_string();
        assert!(error.contains("interrupted"), "{error}");
        assert!(error.contains("cleanup"), "{error}");
        assert_eq!(fixture.cleanups, 1);
        assert!(!fixture.working()?.exists());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
        fixture.inventory(captured.targets().len(), captured.tests());
        let checked = verify_with(captured, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(finish(checked, Err(ProcessError::InvalidOutput)).is_err());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn miri_cleanup_failure_and_linked_native_results_prevent_completion() -> io::Result<()> {
    for fault in [Fault::BlockedCleanup, Fault::LinkedResult] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedCoreMiri::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(captured.targets().len(), captured.tests());
            let checked = verify_with(captured, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert_eq!(fixture.working()?.exists(), fault == Fault::BlockedCleanup);
            Ok(())
        })?;
    }
    Ok(())
}
