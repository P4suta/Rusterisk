use super::{finish_all, finish_target, reserve, verify_with_diagnostics, CommandMode, Scope};
use crate::{
    input_fuzz::{CapturedInputFuzz, FuzzError, LinuxTarget, Phase, Target},
    process::ProcessError,
    release_check::SourceInputs,
};
use std::io;
#[path = "tests/fixture.rs"]
mod fixture;
use fixture::{using_fixture, Fault, IMAGE};

const ARCH: LinuxTarget = LinuxTarget::Arm64;

fn verify_with(
    capture: CapturedInputFuzz,
    scope: Scope,
    image: &str,
    architecture: LinuxTarget,
    perform: impl FnMut(
        &mut std::process::Command,
        std::time::Duration,
        CommandMode,
    ) -> Result<(), ProcessError>,
) -> Result<super::CheckedFuzz, FuzzError> {
    verify_with_diagnostics(
        capture,
        scope,
        image,
        architecture,
        perform,
        &mut Vec::new(),
    )
}

fn diagnostic_case(fault: Fault, phase: &str, expected: &str) -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = fault;
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let mut diagnostics = Vec::new();
        let checked = verify_with_diagnostics(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
            &mut diagnostics,
        );
        assert!(finish_all(checked, Ok(())).is_err());
        let text = std::str::from_utf8(&diagnostics).map_err(io::Error::other)?;
        if !text.contains(phase) || !text.contains(expected) {
            return Err(io::Error::other(format!(
                "native diagnostics missing for {phase}"
            )));
        }
        assert!(diagnostics.len() <= 65536 + 256);
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn diagnostics_keep_native_output_after_process_failure() -> io::Result<()> {
    diagnostic_case(
        Fault::ProcessFailure(Phase::Build(Target::Settings)),
        "phase=build-settings",
        "native build output",
    )
}

#[test]
fn diagnostics_keep_native_output_after_native_completion_rejection() -> io::Result<()> {
    diagnostic_case(Fault::IncompleteRun, "phase=run-settings", "#1001 DONE")
}

#[test]
fn diagnostics_bound_native_bytes_and_keep_write_failures_secondary() -> io::Result<()> {
    let bytes = vec![0xff; 70000];
    let mut output = Vec::new();
    super::write_diagnostic(Phase::Run(Target::Settings), Some(&bytes), &mut output)
        .map_err(io::Error::other)?;
    let text = std::str::from_utf8(&output).map_err(io::Error::other)?;
    assert!(text.contains("phase=run-settings"));
    assert!(text.contains("truncated"));
    assert!(output.len() <= 65536 + 256);
    struct Blocked;
    impl io::Write for Blocked {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("fixture diagnostic blocked"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    using_fixture(|fixture| {
        fixture.fault = Fault::ProcessFailure(Phase::Build(Target::Settings));
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with_diagnostics(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
            &mut Blocked,
        );
        let error = finish_all(checked, Ok(()))
            .err()
            .ok_or_else(|| io::Error::other("diagnostic failure completed"))?
            .to_string();
        assert!(error.contains("process failed"), "{error}");
        assert!(error.contains("diagnostic blocked"), "{error}");
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn all_fuzz_outcomes_require_every_target_and_preserve_captured_inputs() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let key = SourceInputs::new(captured.inputs())
            .map_err(io::Error::other)?
            .key();
        std::fs::write(fixture.root.join("xtask/src/lib.rs"), b"live edit")?;
        let checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        let outcome = finish_all(checked, Ok(())).map_err(io::Error::other)?;
        assert_eq!(outcome.image(), IMAGE);
        assert_eq!(outcome.architecture(), ARCH);
        assert!(matches!(outcome.scope(), Scope::All));
        assert_eq!(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key(),
            key
        );
        assert_eq!(outcome.runs().len(), Target::ALL.len());
        for (run, target) in outcome.runs().iter().zip(Target::ALL) {
            assert_eq!(run.target(), target);
            assert_eq!(run.executions(), fixture::count(target));
        }
        assert_eq!(fixture.phases, fixture::phases(Scope::All));
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn single_target_fuzz_outcomes_cannot_complete_the_all_target_boundary() -> io::Result<()> {
    for target in Target::ALL {
        using_fixture(|fixture| {
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::Target(target),
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            let outcome = finish_target(checked, Ok(())).map_err(io::Error::other)?;
            assert_eq!(outcome.image(), IMAGE);
            assert_eq!(outcome.architecture(), ARCH);
            assert!(matches!(outcome.scope(), Scope::Target(actual) if actual == target));
            assert_eq!(outcome.run().target(), target);
            assert_eq!(outcome.run().executions(), fixture::count(target));
            assert_eq!(fixture.phases, fixture::phases(Scope::Target(target)));
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
        using_fixture(|fixture| {
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::Target(target),
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish_all(checked, Ok(())).is_err());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn native_versions_inventory_and_each_target_completion_are_authoritative() -> io::Result<()> {
    for fault in [
        Fault::WrongRust,
        Fault::WrongArchitecture,
        Fault::WrongFuzzVersion,
        Fault::MissingInventory,
        Fault::IncompleteRun,
        Fault::ZeroRuns,
        Fault::Timeout,
        Fault::Crash,
        Fault::OversizedOutput,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish_all(checked, Ok(())).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn captured_spec_source_and_harness_drift_never_complete() -> io::Result<()> {
    using_fixture(|fixture| {
        let path = fixture.root.join("xtask/fuzz/src/targets.rs");
        let mut bytes = std::fs::read(&path)?;
        bytes.push(b'\n');
        std::fs::write(path, bytes)?;
        assert!(CapturedInputFuzz::read(&fixture.root).is_err());
        Ok(())
    })?;
    for fault in [
        Fault::ChangedSource,
        Fault::AddedSourceConfiguration,
        Fault::ChangedHarness,
        Fault::AddedHarnessSource,
        Fault::AddedHarnessConfiguration,
        Fault::MissingHarness,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish_all(checked, Ok(())).is_err());
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn every_interrupted_phase_owns_container_and_temporary_cleanup() -> io::Result<()> {
    for phase in fixture::phases(Scope::All) {
        using_fixture(|fixture| {
            fixture.fault = Fault::Interrupted(phase);
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            let error = finish_all(checked, Ok(()))
                .err()
                .ok_or_else(|| io::Error::other("interruption completed"))?
                .to_string();
            assert!(error.contains("interrupted"), "{error}");
            assert_eq!(fixture.cleanups, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn every_abnormal_supervised_result_attempts_owned_container_cleanup() -> io::Result<()> {
    for error in [
        ProcessError::Io(io::Error::other("fixture I/O failure")),
        ProcessError::Timeout,
        ProcessError::InvalidBudget,
        ProcessError::InvalidOutput,
        ProcessError::Interrupted,
        ProcessError::UnsupportedSupervision,
        ProcessError::Cleanup {
            operation: "fixture operation".to_owned(),
            cleanup: "fixture process cleanup".to_owned(),
        },
    ] {
        using_fixture(|fixture| {
            let mut error = Some(error);
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| {
                    fixture.perform(command, budget, mode)?;
                    match mode {
                        CommandMode::Work => {
                            Err(error.take().ok_or(ProcessError::InvalidOutput)?)
                        }
                        CommandMode::Cleanup => Ok(()),
                    }
                },
            );
            assert!(finish_all(checked, Ok(())).is_err());
            assert_eq!(fixture.cleanups, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn container_preserve_and_supervisor_failures_keep_the_primary_error() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::Interrupted(Phase::Run(Target::Settings));
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| {
                if mode == CommandMode::Cleanup {
                    fixture.cleanups += 1;
                    Err(ProcessError::InvalidOutput)
                } else {
                    fixture.perform(command, budget, mode)
                }
            },
        );
        let error = finish_all(checked, Err(ProcessError::Interrupted))
            .err()
            .ok_or_else(|| io::Error::other("failed cleanup completed"))?
            .to_string();
        assert!(error.contains("interrupted"), "{error}");
        assert!(error.contains("cleanup"), "{error}");
        assert!(!fixture.working()?.exists());
        Ok(())
    })?;
    using_fixture(|fixture| {
        fixture.fault = Fault::PreserveFailure;
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        let error = finish_all(checked, Ok(()))
            .err()
            .ok_or_else(|| io::Error::other("failed preservation completed"))?
            .to_string();
        assert!(error.contains("interrupted"), "{error}");
        assert!(error.contains("preserv"), "{error}");
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn failing_inputs_keep_the_existing_bounded_preservation_layout() -> io::Result<()> {
    for fault in [Fault::Artifact, Fault::Crash] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish_all(checked, Ok(())).is_err());
            let identity = fixture
                .identity
                .as_deref()
                .ok_or_else(|| io::Error::other("identity missing"))?;
            assert_eq!(
                std::fs::read(
                    fixture
                        .root
                        .join(".cache/input-fuzz-crashes")
                        .join(identity)
                        .join("settings/crash-deadbeef")
                )?,
                b"failing input"
            );
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn failing_input_preservation_refuses_invalid_names_sizes_and_excess_entries() -> io::Result<()> {
    for fault in [
        Fault::InvalidArtifact,
        Fault::OversizedArtifact,
        Fault::ExcessArtifacts,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            let error = finish_all(checked, Ok(()))
                .err()
                .ok_or_else(|| io::Error::other("invalid preservation completed"))?
                .to_string();
            assert!(error.contains("input fuzz requires"), "{error}");
            assert!(error.contains("preserv"), "{error}");
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    using_fixture(|fixture| {
        let directory = fixture.root.join("owned");
        let name = "x".repeat(160);
        let source = directory.join("state/artifacts/settings");
        std::fs::create_dir_all(&source)?;
        let bytes = vec![b'x'; 65536];
        std::fs::write(source.join(&name), &bytes)?;
        super::preserve(&fixture.root, &directory, "preservation", Target::Settings)
            .map_err(io::Error::other)?;
        let destination = fixture
            .root
            .join(".cache/input-fuzz-crashes/preservation/settings")
            .join(&name);
        assert_eq!(std::fs::read(&destination)?, bytes);
        std::fs::write(&destination, b"existing")?;
        assert!(
            super::preserve(&fixture.root, &directory, "preservation", Target::Settings).is_err()
        );
        assert_eq!(std::fs::read(destination)?, b"existing");
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn failing_inputs_never_follow_a_linked_preservation_cache() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::LinkedCache;
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        let error = finish_all(checked, Ok(()))
            .err()
            .ok_or_else(|| io::Error::other("linked preservation completed"))?
            .to_string();
        assert!(error.contains("interrupted"), "{error}");
        assert!(error.contains("preserv"), "{error}");
        assert!(std::fs::read_dir(fixture.root.join("outside-cache"))?
            .next()
            .is_none());
        assert!(!fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn supervisor_and_incomplete_all_target_counts_never_complete() -> io::Result<()> {
    using_fixture(|fixture| {
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
        );
        assert!(finish_all(checked, Err(ProcessError::InvalidOutput)).is_err());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
        let mut checked = verify_with(
            captured,
            Scope::All,
            IMAGE,
            ARCH,
            |command, budget, mode| fixture.perform(command, budget, mode),
        )
        .map_err(io::Error::other)?;
        assert!(checked.runs.pop().is_some());
        assert!(finish_all(Ok(checked), Ok(())).is_err());
        Ok(())
    })
}

#[test]
fn fuzz_reservation_preserves_existing_names_and_bounds_attempts() -> io::Result<()> {
    using_fixture(|fixture| {
        let mut directories = Vec::new();
        for attempt in 0..32 {
            let (identity, path) =
                reserve(&fixture.root, "reservation").map_err(io::Error::other)?;
            assert_eq!(
                identity,
                if attempt == 0 {
                    "reservation".to_owned()
                } else {
                    format!("reservation-{attempt}")
                }
            );
            std::fs::write(path.join("sentinel"), b"owned")?;
            directories.push(path);
        }
        assert!(reserve(&fixture.root, "reservation").is_err());
        for path in directories {
            assert_eq!(std::fs::read(path.join("sentinel"))?, b"owned");
        }
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn linked_native_outputs_and_failed_temp_cleanup_never_complete() -> io::Result<()> {
    for fault in [Fault::LinkedOutput, Fault::BlockedCleanup] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let captured = CapturedInputFuzz::read(&fixture.root).map_err(io::Error::other)?;
            let checked = verify_with(
                captured,
                Scope::All,
                IMAGE,
                ARCH,
                |command, budget, mode| fixture.perform(command, budget, mode),
            );
            assert!(finish_all(checked, Ok(())).is_err());
            assert_eq!(fixture.working()?.exists(), fault == Fault::BlockedCleanup);
            Ok(())
        })?;
    }
    Ok(())
}
