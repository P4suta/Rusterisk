use super::{finish, reserve, verify_with, CommandMode};
use crate::{
    core_assurance::{CapturedAssurance, FieldMutation, Phase},
    core_mutation::CapturedCoreMutation,
    process::ProcessError,
    release_check::SourceInputs,
};
use std::io;
#[path = "tests/fixture.rs"]
mod fixture;
use fixture::{using_fixture, Fault, IMAGE, PHASES};

#[test]
fn mutation_uses_the_authoritative_source_manifest_and_test_inventory() -> io::Result<()> {
    using_fixture(|fixture| {
        let shared =
            CapturedAssurance::read(&fixture.root.join("crates/core")).map_err(io::Error::other)?;
        let captured = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
        assert_eq!(captured.targets(), shared.targets());
        assert_eq!(captured.tests(), shared.tests());
        assert_eq!(captured.targets().len(), 24);
        assert_eq!(captured.tests().len(), 69);
        let plan = crate::core_mutation::plan(IMAGE).map_err(io::Error::other)?;
        for phase in PHASES {
            assert!(plan
                .step(phase)
                .arguments
                .iter()
                .any(|argument| argument == IMAGE));
        }
        assert!(fixture.temporary.is_none());
        Ok(())
    })
}

#[test]
fn mutation_completion_requires_generated_and_both_supplementary_native_results() -> io::Result<()>
{
    using_fixture(|fixture| {
        let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
        let key = SourceInputs::new(capture.inputs())
            .map_err(io::Error::other)?
            .key();
        fixture.inventory(capture.targets().len(), capture.tests());
        std::fs::write(fixture.root.join("crates/core/src/lib.rs"), b"live edit")?;
        let checked = verify_with(capture, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        let outcome = finish(checked, Ok(())).map_err(io::Error::other)?;
        assert_eq!(outcome.image(), IMAGE);
        assert_eq!(outcome.test_count(), 69);
        assert_eq!(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key(),
            key
        );
        assert!(fixture.phases == PHASES);
        assert!(!fixture.working()?.exists());
        assert!(crate::core_assurance::validate_mutation_outcomes(
            &fixture::campaign(Fault::None)?,
            outcome.inventory()
        )
        .is_err());
        Ok(())
    })
}

#[test]
fn mutation_rejects_missing_changed_truncated_and_incomplete_generated_results() -> io::Result<()> {
    for fault in [
        Fault::MissingInventory,
        Fault::MissingCampaign,
        Fault::ChangedInventory,
        Fault::TruncatedCampaign,
        Fault::Missed,
        Fault::Timeout,
        Fault::WrongVersion,
        Fault::OversizedCampaign,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(capture.targets().len(), capture.tests());
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
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
fn mutation_requires_each_supplement_to_build_and_reject_complete_tests_with_exit101(
) -> io::Result<()> {
    for mutation in FieldMutation::ALL {
        for fault in [
            Fault::FailedBuild(mutation),
            Fault::Survived(mutation),
            Fault::WrongExit(mutation),
            Fault::MissingTests(mutation),
            Fault::SkippedTests(mutation),
            Fault::TruncatedTests(mutation),
        ] {
            using_fixture(|fixture| {
                fixture.fault = fault;
                let capture =
                    CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
                fixture.inventory(capture.targets().len(), capture.tests());
                let checked = verify_with(capture, IMAGE, |command, budget, mode| {
                    fixture.perform(command, budget, mode)
                });
                assert!(finish(checked, Ok(())).is_err());
                assert!(!fixture.working()?.exists());
                if fault == Fault::FailedBuild(mutation) {
                    assert!(!fixture.phases.contains(&Phase::FieldTests(mutation)));
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

#[test]
fn mutation_rejects_changed_original_normalized_and_field_trees() -> io::Result<()> {
    for fault in [
        Fault::ChangedOriginal,
        Fault::ChangedWorking,
        Fault::ChangedManifest,
        Fault::ChangedField(FieldMutation::TerminalState),
        Fault::ChangedField(FieldMutation::ConnectionClaim),
        Fault::ChangedPriorField,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(capture.targets().len(), capture.tests());
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
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
fn mutation_interruptions_own_each_container_and_preserve_cleanup_failures() -> io::Result<()> {
    for phase in PHASES {
        using_fixture(|fixture| {
            fixture.fault = Fault::Interrupted(phase);
            let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(capture.targets().len(), capture.tests());
            let checked = verify_with(capture, IMAGE, |command, budget, mode| match mode {
                CommandMode::Work => fixture.perform(command, budget, mode),
                CommandMode::Cleanup => {
                    assert_eq!(budget.as_secs(), 30);
                    let args: Vec<_> = command.get_args().collect();
                    assert_eq!(args.len(), 3);
                    assert_eq!(args[0], "rm");
                    assert_eq!(args[1], "--force");
                    assert!(args[2].to_string_lossy().ends_with(phase.label()));
                    fixture.cleanups += 1;
                    Err(ProcessError::InvalidOutput)
                }
            });
            let error = finish(checked, Err(ProcessError::Interrupted))
                .err()
                .ok_or_else(|| io::Error::other("interrupted mutation completed"))?
                .to_string();
            assert!(error.contains("interrupted"), "{error}");
            assert!(error.contains("cleanup"), "{error}");
            assert_eq!(fixture.cleanups, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn mutation_supervisor_failure_and_invalid_images_never_create_outcomes() -> io::Result<()> {
    using_fixture(|fixture| {
        let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
        fixture.inventory(capture.targets().len(), capture.tests());
        let checked = verify_with(capture, IMAGE, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(finish(checked, Err(ProcessError::InvalidOutput)).is_err());
        assert!(!fixture.working()?.exists());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
        assert!(finish(
            verify_with(capture, "mutable:tag", |command, budget, mode| {
                fixture.perform(command, budget, mode)
            }),
            Ok(())
        )
        .is_err());
        assert!(fixture.phases.is_empty());
        assert!(fixture.temporary.is_none());
        Ok(())
    })
}

#[test]
fn mutation_reservation_preserves_existing_owned_names_and_bounds_attempts() -> io::Result<()> {
    using_fixture(|fixture| {
        let mut directories = Vec::new();
        for attempt in 0..32 {
            let (identity, directory) =
                reserve(&fixture.root, "reservation").map_err(io::Error::other)?;
            assert_eq!(
                identity,
                if attempt == 0 {
                    "reservation".to_owned()
                } else {
                    format!("reservation-{attempt}")
                }
            );
            std::fs::write(directory.join("sentinel"), b"owned")?;
            directories.push(directory);
        }
        assert!(reserve(&fixture.root, "reservation").is_err());
        for directory in directories {
            assert_eq!(std::fs::read(directory.join("sentinel"))?, b"owned");
        }
        Ok(())
    })
}

#[cfg(unix)]
#[test]
fn mutation_rejects_linked_native_files_directories_and_failed_temp_cleanup() -> io::Result<()> {
    for fault in [
        Fault::LinkedCampaign,
        Fault::LinkedResultsDirectory,
        Fault::BlockedCleanup,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let capture = CapturedCoreMutation::read(&fixture.root).map_err(io::Error::other)?;
            fixture.inventory(capture.targets().len(), capture.tests());
            let checked = verify_with(capture, IMAGE, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            assert!(finish(checked, Ok(())).is_err());
            assert_eq!(fixture.working()?.exists(), fault == Fault::BlockedCleanup);
            Ok(())
        })?;
    }
    Ok(())
}
