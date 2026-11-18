#[path = "sdk_content/fixture.rs"]
mod archive_fixture;
#[path = "sdk_contents/fixture.rs"]
mod candidate_fixture;
#[path = "sdk_candidate_build/fixture.rs"]
mod captured_fixture;

use std::{num::NonZeroUsize, time::Duration};
use xtask::{
    distribution::{
        verify_sdk_consumers_with, verify_sdk_consumers_with_jobs, verify_sdk_with,
        verify_sdk_with_jobs, SdkContents,
    },
    process::{ProcessError, SupervisedCommand},
};

#[test]
fn a_failed_owned_command_stops_verification_preserves_diagnostics_and_reaps_the_working_copy(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "consumer-run-failure",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let execution = candidate.with_file_name("execution");
            let artifacts = candidate.with_file_name("artifacts");
            let mut calls = 0;
            let outcome =
                verify_sdk_consumers_with(&contents, &execution, &artifacts, |command, budget| {
                    let command = SupervisedCommand::into_std(command);
                    calls += 1;
                    assert_eq!(budget, Duration::from_secs(180));
                    assert_eq!(command.get_program(), "rustup");
                    let cwd = command
                        .get_current_dir()
                        .ok_or_else(|| std::io::Error::other("command cwd missing"))?;
                    assert_eq!(cwd, execution.join("work/consumers/core-msrv"));
                    let environments: std::collections::BTreeMap<_, _> =
                        command.get_envs().collect();
                    assert_eq!(
                        environments
                            .get(std::ffi::OsStr::new("CARGO_BUILD_JOBS"))
                            .copied()
                            .flatten(),
                        Some(std::ffi::OsStr::new("2"))
                    );
                    std::fs::write(
                        execution.join("state/core-msrv-resolve.stderr.txt"),
                        b"fixture command diagnostic\n",
                    )?;
                    Err(ProcessError::Interrupted)
                });
            assert!(outcome.is_err());
            assert_eq!(calls, 1);
            assert!(!execution.exists());
            assert_eq!(
                std::fs::read(artifacts.join("core-msrv-resolve.stderr.txt"))?,
                b"fixture command diagnostic\n"
            );
            assert_eq!(
                std::fs::read_to_string(artifacts.join("SOURCE.sha256"))?,
                format!("{}\n", contents.source_digest())
            );
            Ok(())
        },
    )
}

#[test]
fn explicit_jobs_reach_the_owned_command_and_failure_cleanup() -> std::io::Result<()> {
    candidate_fixture::using_candidate(
        "consumer-single-job-failure",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let execution = candidate.with_file_name("execution");
            let artifacts = candidate.with_file_name("artifacts");
            let mut calls = 0;
            let result = verify_sdk_consumers_with_jobs(
                &contents,
                &execution,
                &artifacts,
                NonZeroUsize::MIN,
                |command, budget| {
                    calls += 1;
                    let command = command.into_std();
                    assert_eq!(command.get_program(), "rustup");
                    assert_eq!(budget, Duration::from_secs(180));
                    assert_eq!(
                        command
                            .get_envs()
                            .find(|(name, _)| *name == "CARGO_BUILD_JOBS")
                            .and_then(|(_, value)| value),
                        Some(std::ffi::OsStr::new("1"))
                    );
                    std::fs::write(
                        execution.join("state/core-msrv-resolve.stderr.txt"),
                        b"single-job failure\n",
                    )?;
                    Err(ProcessError::Interrupted)
                },
            );
            assert!(result.is_err());
            assert_eq!(calls, 1);
            assert!(!execution.exists());
            assert_eq!(
                std::fs::read(artifacts.join("core-msrv-resolve.stderr.txt"))?,
                b"single-job failure\n"
            );
            Ok(())
        },
    )
}

#[test]
fn explicit_jobs_reach_the_checkout_verification_entry() -> std::io::Result<()> {
    candidate_fixture::using_candidate(
        "consumer-checkout-single-job",
        |_| Ok(()),
        |candidate| {
            let root = candidate.with_file_name("checkout");
            std::fs::create_dir(&root)?;
            let mut execution = None;
            let result = verify_sdk_with_jobs(&root, candidate, NonZeroUsize::MIN, |command, _| {
                let command = command.into_std();
                assert_eq!(
                    command
                        .get_envs()
                        .find(|(name, _)| *name == "CARGO_BUILD_JOBS")
                        .and_then(|(_, value)| value),
                    Some(std::ffi::OsStr::new("1"))
                );
                execution = Some(
                    command
                        .get_current_dir()
                        .and_then(|directory| directory.ancestors().nth(3))
                        .ok_or_else(|| std::io::Error::other("consumer execution root missing"))?
                        .to_owned(),
                );
                Err(ProcessError::Interrupted)
            });
            assert!(result.is_err());
            let execution = execution.ok_or_else(|| {
                std::io::Error::other("checkout verification skipped its consumer")
            })?;
            assert!(!execution.exists());
            Ok(())
        },
    )
}

#[test]
fn a_successful_port_cannot_change_sources_or_resolve_unapproved_dependencies(
) -> Result<(), std::io::Error> {
    for mutate_source in [false, true] {
        candidate_fixture::using_candidate(
            "consumer-run-invalid",
            |_| Ok(()),
            |candidate| {
                let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
                let execution = candidate.with_file_name("execution");
                let artifacts = candidate.with_file_name("artifacts");
                let mut calls = 0;
                let outcome =
                    verify_sdk_consumers_with(&contents, &execution, &artifacts, |_, _| {
                        calls += 1;
                        let path = if mutate_source {
                            "work/packages/rusterisk-core-0.1.0/src/lib.rs"
                        } else {
                            "work/consumers/core-msrv/Cargo.lock"
                        };
                        std::fs::write(execution.join(path), b"changed\n")?;
                        Ok(())
                    });
                assert!(outcome.is_err());
                assert_eq!(calls, 1);
                assert!(!execution.exists());
                Ok(())
            },
        )?;
    }
    Ok(())
}

#[test]
fn consumer_verification_rejects_invalid_candidates_before_creating_cache_state(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "consumer-invalid-candidate",
        |_| Ok(()),
        |candidate| {
            let root = candidate.with_file_name("checkout");
            std::fs::create_dir(&root)?;
            let mut calls = 0;
            let outcome = verify_sdk_with(&root, &candidate.join("missing"), |_, _| {
                calls += 1;
                Ok(())
            });
            assert!(outcome.is_err());
            assert_eq!(calls, 0);
            assert!(!root.join(".cache").exists());
            Ok(())
        },
    )
}

#[test]
fn existing_artifacts_cannot_be_replaced_or_nested_in_the_execution_copy(
) -> Result<(), std::io::Error> {
    candidate_fixture::using_candidate(
        "consumer-run-owned",
        |_| Ok(()),
        |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let execution = candidate.with_file_name("execution");
            let artifacts = candidate.with_file_name("artifacts");
            std::fs::create_dir(&artifacts)?;
            std::fs::write(artifacts.join("user-data"), b"preserve")?;
            let mut calls = 0;
            let outcome = verify_sdk_consumers_with(&contents, &execution, &artifacts, |_, _| {
                calls += 1;
                Ok(())
            });
            assert!(outcome.is_err());
            assert_eq!(calls, 0);
            assert!(!execution.exists());
            assert_eq!(std::fs::read(artifacts.join("user-data"))?, b"preserve");
            assert!(verify_sdk_consumers_with(
                &contents,
                &execution,
                &execution.join("artifacts"),
                |_, _| Ok(())
            )
            .is_err());
            Ok(())
        },
    )
}
