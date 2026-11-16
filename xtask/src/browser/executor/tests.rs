use super::{finish, run_prepared_with, BrowserCommand, PreparedBrowserRun, RunError};
use crate::{browser::Phase, process::ProcessError, release_check::SourceInputs};
use std::io;

mod fixture;
use fixture::{using_fixture, Fault};

fn prepared_in_fixture(fixture: &fixture::Fixture) -> io::Result<PreparedBrowserRun> {
    let mut prepared =
        PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
    prepared.directory = fixture.root.join("planned");
    prepared.plan = crate::browser::RunPlan::new(
        &prepared.identity,
        prepared
            .directory
            .join("source")
            .to_str()
            .ok_or_else(|| io::Error::other("fixture source path"))?,
        prepared
            .directory
            .join("state")
            .to_str()
            .ok_or_else(|| io::Error::other("fixture state path"))?,
        &fixture.image,
    )
    .map_err(io::Error::other)?;
    Ok(prepared)
}

#[test]
fn preparing_and_displaying_a_browser_plan_never_executes() -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        assert!(prepared
            .plan()
            .step(Phase::Run)
            .arguments
            .contains(&fixture.image));
        let mut commands = 0;
        super::execute_with(
            &fixture.root,
            super::Execution::DryRun,
            &fixture.image,
            |_, _| {
                commands += 1;
                Ok(())
            },
        )
        .map_err(io::Error::other)?;
        assert_eq!(commands, 0);
        assert!(!fixture.root.join(".cache").exists());
        Ok(())
    })
}

#[test]
fn browser_outcomes_bind_the_executed_inputs_image_inventory_and_preserved_native_output(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let expected = SourceInputs::new(prepared.inputs()).map_err(io::Error::other)?;
        let expected_key = expected.key();
        drop(expected);
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        let outcome = finish(checked, Ok(())).map_err(io::Error::other)?;
        assert!(!fixture.working()?.exists());
        assert_eq!(fixture.phases, Phase::ALL);
        assert_eq!(outcome.image(), fixture.image);
        assert_eq!(outcome.inventory().count(), 4);
        assert_eq!(
            SourceInputs::new(outcome.inputs())
                .map_err(io::Error::other)?
                .key(),
            expected_key
        );
        assert!(outcome.artifacts().join("browser-results.json").is_file());
        assert!(outcome.artifacts().join("run.txt").is_file());
        Ok(())
    })
}

#[test]
fn successful_commands_cannot_replace_native_results_or_captured_source_checks() -> io::Result<()> {
    for fault in [
        Fault::MissingNative,
        Fault::SkippedNative,
        Fault::ChangedSource,
    ] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let prepared = PreparedBrowserRun::read(&fixture.root, &fixture.image)
                .map_err(io::Error::other)?;
            let checked = run_prepared_with(prepared, |command, budget, mode| {
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
fn preservation_and_supervisor_cleanup_failures_cannot_produce_a_browser_outcome() -> io::Result<()>
{
    using_fixture(|fixture| {
        fixture.fault = Fault::BlockedArtifacts;
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(finish(checked, Ok(())).is_err());
        assert!(!fixture.working()?.exists());
        Ok(())
    })?;
    using_fixture(|fixture| {
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(matches!(
            finish(checked, Err(ProcessError::InvalidOutput)),
            Err(RunError::Process(ProcessError::InvalidOutput))
        ));
        Ok(())
    })
}

#[test]
fn timeout_and_interruption_preserve_the_primary_failure_and_use_owned_cleanup() -> io::Result<()> {
    for fault in [Fault::Timeout, Fault::Interrupted] {
        using_fixture(|fixture| {
            fixture.fault = fault;
            let prepared = PreparedBrowserRun::read(&fixture.root, &fixture.image)
                .map_err(io::Error::other)?;
            let checked = run_prepared_with(prepared, |command, budget, mode| {
                fixture.perform(command, budget, mode)
            });
            let outcome = finish(checked, Err(ProcessError::Interrupted));
            let Err(RunError::SupervisorCleanup { operation, cleanup }) = outcome else {
                return Err(io::Error::other(
                    "primary and supervisor cleanup failures were lost",
                ));
            };
            assert!(operation.contains(if fault == Fault::Timeout {
                "deadline exceeded"
            } else {
                "interrupted"
            }));
            assert!(matches!(cleanup, ProcessError::Interrupted));
            assert_eq!(fixture.cleanups, 1);
            assert!(!fixture.working()?.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_temporary_directory_cleanup_cannot_produce_a_browser_outcome() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::BlockedCleanup;
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(finish(checked, Ok(())).is_err());
        assert!(fixture.working()?.exists());
        Ok(())
    })
}

#[test]
fn colliding_planned_names_are_reserved_atomically_without_using_or_removing_foreign_state(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let prepared = prepared_in_fixture(fixture)?;
        let foreign = prepared.directory.clone();
        std::fs::create_dir(&foreign)?;
        std::fs::write(foreign.join("foreign"), b"keep")?;
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        finish(checked, Ok(())).map_err(io::Error::other)?;
        assert_ne!(fixture.working()?, foreign);
        assert!(!fixture.working()?.exists());
        assert_eq!(std::fs::read(foreign.join("foreign"))?, b"keep");
        Ok(())
    })
}

#[test]
fn exhausted_directory_reservations_refuse_to_execute_or_modify_existing_entries() -> io::Result<()>
{
    using_fixture(|fixture| {
        let prepared = prepared_in_fixture(fixture)?;
        let mut foreign = Vec::new();
        for attempt in 0..32 {
            let path = if attempt == 0 {
                prepared.directory.clone()
            } else {
                fixture.root.join(format!(
                    "rusterisk-browser-verify-{}-{attempt}",
                    prepared.identity
                ))
            };
            std::fs::create_dir(&path)?;
            std::fs::write(path.join("foreign"), b"keep")?;
            foreign.push(path);
        }
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        assert!(
            matches!(finish(checked, Ok(())), Err(RunError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert!(fixture.phases.is_empty());
        assert_eq!(fixture.cleanups, 0);
        for path in foreign {
            assert_eq!(std::fs::read(path.join("foreign"))?, b"keep");
        }
        Ok(())
    })
}

#[test]
fn container_cleanup_errors_remain_attached_to_the_interrupted_operation() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::Interrupted;
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let checked = run_prepared_with(prepared, |command, budget, mode| match mode {
            BrowserCommand::Run => fixture.perform(command, budget, mode),
            BrowserCommand::Cleanup => Err(ProcessError::InvalidOutput),
        });
        assert!(matches!(
            finish(checked, Ok(())),
            Err(RunError::ContainerCleanup {
                cleanup: ProcessError::InvalidOutput,
                ..
            })
        ));
        Ok(())
    })
}

#[test]
fn container_cleanup_output_failures_do_not_erase_the_interrupted_operation() -> io::Result<()> {
    using_fixture(|fixture| {
        fixture.fault = Fault::CleanupOutputCollision;
        let prepared =
            PreparedBrowserRun::read(&fixture.root, &fixture.image).map_err(io::Error::other)?;
        let checked = run_prepared_with(prepared, |command, budget, mode| {
            fixture.perform(command, budget, mode)
        });
        let Err(RunError::ContainerCleanup { operation, cleanup }) = finish(checked, Ok(())) else {
            return Err(io::Error::other(
                "cleanup output creation erased the primary failure",
            ));
        };
        assert!(operation.contains("interrupted"));
        assert!(
            matches!(cleanup, ProcessError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(fixture.cleanups, 1);
        Ok(())
    })
}
