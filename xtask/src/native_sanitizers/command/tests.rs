use super::{finish, plan, CapturedNative, CommandMode, NativeError, Scope};
use crate::{process::ProcessError, release_check::SourceInputs};
use std::io;

mod fixture;
use fixture::{using_fixture, Fault, IMAGE, PHASES};

#[test]
fn native_planning_has_no_execution_or_result_directory() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let plan = plan(IMAGE).map_err(io::Error::other)?;
        assert!(plan
            .step(PHASES[0])
            .arguments
            .iter()
            .any(|argument| argument == IMAGE));
        assert!(!fixture.destination.exists());
        assert_eq!(
            captured
                .inputs()
                .find(|(path, _)| *path == std::path::Path::new("crates/core/src/lib.rs"))
                .map(|(_, bytes)| bytes),
            Some(b"fixture".as_slice())
        );
        Ok(())
    })
}

#[test]
fn native_outcomes_keep_every_scope_and_bind_source_image_and_existing_artifacts() -> io::Result<()>
{
    using_fixture(|fixture| {
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let expected = SourceInputs::new(captured.inputs())
            .map_err(io::Error::other)?
            .key();
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        let outcome = finish(captured, checked, Ok(()), fixture.destination.clone())
            .map_err(io::Error::other)?;
        assert_eq!(fixture.phases, PHASES);
        assert_eq!(outcome.image(), IMAGE);
        assert_eq!(outcome.test_count(), 8);
        assert_eq!(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key(),
            expected
        );
        for scope in Scope::ALL {
            assert!(outcome
                .inventory(scope)
                .validate(fixture::execution(scope).as_bytes())
                .is_ok());
        }
        assert!(outcome.artifacts().join("SOURCE.sha256").is_file());
        assert!(outcome
            .artifacts()
            .join("source/crates/core/src/lib.rs")
            .is_file());
        assert!(outcome.artifacts().join("inspect-opus.txt").is_file());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn invalid_native_probes_archives_or_instrumentation_cannot_produce_outcomes() -> io::Result<()> {
    for fault in [
        Fault::WrongProbe,
        Fault::AcceptedProbe,
        Fault::MissingArchive,
        Fault::MissingSymbols,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
            let checked = captured.run_checked_with(
                &fixture.destination.clone(),
                IMAGE,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish(captured, checked, Ok(()), fixture.destination.clone()).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn every_scope_requires_complete_inventory_and_execution() -> io::Result<()> {
    for fault in [
        Fault::MissingInventory(Scope::Media),
        Fault::MissingInventory(Scope::Storage),
        Fault::MissingInventory(Scope::Tls),
        Fault::SkippedExecution,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
            let checked = captured.run_checked_with(
                &fixture.destination.clone(),
                IMAGE,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish(captured, checked, Ok(()), fixture.destination.clone()).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn changed_execution_or_preserved_inputs_cannot_receive_a_native_outcome() -> io::Result<()> {
    for fault in [Fault::ChangedWorking, Fault::ChangedPreserved] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
            let checked = captured.run_checked_with(
                &fixture.destination.clone(),
                IMAGE,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish(captured, checked, Ok(()), fixture.destination.clone()).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn native_supervisor_cleanup_must_succeed_even_after_complete_execution() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        assert!(finish(
            captured,
            checked,
            Err(ProcessError::InvalidOutput),
            fixture.destination.clone()
        )
        .is_err());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn interrupted_native_work_keeps_its_primary_and_owned_cleanup_failures() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::Interrupted;
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| match mode {
                CommandMode::Work => fixture.perform(command, budget, mode),
                CommandMode::Cleanup => Err(ProcessError::InvalidOutput),
            },
        );
        let error = finish(
            captured,
            checked,
            Err(ProcessError::Interrupted),
            fixture.destination.clone(),
        )
        .err()
        .ok_or_else(|| io::Error::other("interruption was accepted"))?
        .to_string();
        assert!(error.contains("interrupted"), "{error}");
        assert!(error.contains("native container cleanup failed"), "{error}");
        assert!(
            error.contains("native supervisor cleanup failed"),
            "{error}"
        );
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn native_temporary_cleanup_failure_prevents_completion() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::BlockedCleanup;
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        assert!(finish(captured, checked, Ok(()), fixture.destination.clone()).is_err());
        assert!(fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn native_result_directories_are_never_reused_or_replaced() -> io::Result<()> {
    using_fixture(|fixture| {
        std::fs::create_dir(&fixture.destination)?;
        std::fs::write(fixture.destination.join("foreign"), b"keep")?;
        let captured = CapturedNative::read(fixture.root).map_err(io::Error::other)?;
        let checked = captured.run_checked_with(
            &fixture.destination.clone(),
            IMAGE,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        assert!(
            matches!(finish(captured,checked,Ok(()),fixture.destination.clone()), Err(NativeError::Io(error)) if error.kind()==io::ErrorKind::AlreadyExists)
        );
        assert!(fixture.phases.is_empty());
        assert_eq!(std::fs::read(fixture.destination.join("foreign"))?, b"keep");
        Ok(())
    })
}
