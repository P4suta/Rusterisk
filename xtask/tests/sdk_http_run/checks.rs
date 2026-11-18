use super::fixture::{candidate, perform_preparation};
use std::{ffi::OsStr, time::Duration};
use xtask::{
    distribution::{PreparedSdkHttpConsumer, SdkContents, SdkHttpTarget},
    process::ProcessError,
};

#[test]
fn preparation_builds_before_server_start_and_a_single_http_run_owns_its_workspace(
) -> Result<(), std::io::Error> {
    candidate("prepared-http", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phases = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phases += 1;
                perform_preparation(&execution, phases, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        assert_eq!(phases, 3);
        assert!(execution.is_dir());
        assert_eq!(prepared.artifacts(), artifacts);
        let address = "127.0.0.1:8088".parse().map_err(std::io::Error::other)?;
        let target = SdkHttpTarget::new(address, "api", "fixture-password", "0.1.0", "sdk-run")
            .map_err(std::io::Error::other)?;
        let mut runs = 0;
        let returned = prepared
            .run_with(&target, |command, budget| {
                let command = command.into_std();
                runs += 1;
                assert_eq!(budget, Duration::from_secs(60));
                assert!(command.get_args().next().is_none());
                let variables: std::collections::BTreeMap<_, _> = command.get_envs().collect();
                assert_eq!(
                    variables
                        .get(OsStr::new("RUSTERISK_SDK_HTTP_PASSWORD"))
                        .copied()
                        .flatten(),
                    Some(OsStr::new("fixture-password"))
                );
                std::fs::write(
                    execution.join("state/sdk-run.stdout.txt"),
                    b"SDK HTTP candidate consumer passed\n",
                )?;
                Ok(())
            })
            .map_err(std::io::Error::other)?;
        assert_eq!(runs, 1);
        assert_eq!(returned, artifacts);
        assert!(!execution.exists());
        assert_eq!(
            std::fs::read(artifacts.join("sdk-run.stdout.txt"))?,
            b"SDK HTTP candidate consumer passed\n"
        );
        Ok(())
    })
}

#[test]
fn failed_preparation_preserves_its_diagnostic_and_closes_the_working_copy(
) -> Result<(), std::io::Error> {
    candidate("prepare-interrupted", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut calls = 0;
        let result =
            PreparedSdkHttpConsumer::prepare_with(&contents, &execution, &artifacts, |_, _| {
                calls += 1;
                std::fs::write(
                    execution.join("state/sdk-resolve.stderr.txt"),
                    b"injected preparation failure\n",
                )?;
                Err(ProcessError::Interrupted)
            });
        assert!(result.is_err());
        assert_eq!(calls, 1);
        assert!(!execution.exists());
        assert_eq!(
            std::fs::read(artifacts.join("sdk-resolve.stderr.txt"))?,
            b"injected preparation failure\n"
        );
        Ok(())
    })
}

#[test]
fn a_server_start_failure_can_explicitly_close_a_prepared_consumer_without_running_http(
) -> Result<(), std::io::Error> {
    candidate("prepared-close", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phase = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phase += 1;
                perform_preparation(&execution, phase, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        prepared.close().map_err(std::io::Error::other)?;
        assert_eq!(phase, 3);
        assert!(!execution.exists());
        assert!(!artifacts.join("sdk-run.stdout.txt").exists());
        assert!(artifacts.join("sdk-metadata.stdout.json").is_file());
        Ok(())
    })
}

#[test]
fn an_interrupted_http_child_still_preserves_logs_and_removes_only_its_working_copy(
) -> Result<(), std::io::Error> {
    candidate("http-interrupted", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phase = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phase += 1;
                perform_preparation(&execution, phase, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        let target = SdkHttpTarget::new(
            "127.0.0.1:8088".parse().map_err(std::io::Error::other)?,
            "api",
            "fixture-password",
            "0.1.0",
            "sdk-run",
        )
        .map_err(std::io::Error::other)?;
        let result = prepared.run_with(&target, |_, _| Err(ProcessError::Interrupted));
        assert!(result.is_err());
        assert!(!execution.exists());
        assert!(artifacts.join("sdk-run.stderr.txt").is_file());
        assert!(candidate.is_dir());
        Ok(())
    })
}

#[test]
fn rejected_metadata_stops_before_building_and_closes_the_owned_copy() -> Result<(), std::io::Error>
{
    candidate("http-bad-metadata", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phases = 0;
        let result = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phases += 1;
                perform_preparation(&execution, phases, command, budget)?;
                if phases == 2 {
                    let path = execution.join("state/sdk-metadata.stdout.json");
                    let mut data: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path)?)
                            .map_err(std::io::Error::other)?;
                    let source = data
                        .pointer_mut("/packages/4/source")
                        .ok_or_else(|| std::io::Error::other("fixture runtime source missing"))?;
                    *source = serde_json::Value::String("registry+unapproved-source".to_owned());
                    std::fs::write(
                        path,
                        serde_json::to_vec(&data).map_err(std::io::Error::other)?,
                    )?;
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(phases, 2);
        assert!(!execution.exists());
        Ok(())
    })
}

#[test]
fn changed_locks_or_package_sources_cannot_reach_the_http_runtime() -> Result<(), std::io::Error> {
    for relative in [
        "work/consumers/sdk/Cargo.lock",
        "work/packages/rusterisk-client-0.1.0/src/lib.rs",
    ] {
        candidate("http-mutated-copy", |candidate| {
            let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
            let execution = candidate.with_file_name("execution");
            let artifacts = candidate.with_file_name("artifacts");
            let mut phase = 0;
            let prepared = PreparedSdkHttpConsumer::prepare_with(
                &contents,
                &execution,
                &artifacts,
                |command, budget| {
                    phase += 1;
                    perform_preparation(&execution, phase, command, budget)
                },
            )
            .map_err(std::io::Error::other)?;
            std::fs::write(execution.join(relative), b"changed")?;
            let target = SdkHttpTarget::new(
                "127.0.0.1:8088".parse().map_err(std::io::Error::other)?,
                "api",
                "fixture-password",
                "0.1.0",
                "sdk-run",
            )
            .map_err(std::io::Error::other)?;
            let mut calls = 0;
            let result = prepared.run_with(&target, |_, _| {
                calls += 1;
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(calls, 0);
            assert!(!execution.exists());
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn offline_success_output_cannot_satisfy_the_http_contract() -> Result<(), std::io::Error> {
    candidate("http-wrong-marker", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phase = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phase += 1;
                perform_preparation(&execution, phase, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        let target = SdkHttpTarget::new(
            "127.0.0.1:8088".parse().map_err(std::io::Error::other)?,
            "api",
            "fixture-password",
            "0.1.0",
            "sdk-run",
        )
        .map_err(std::io::Error::other)?;
        let result = prepared.run_with(&target, |_, _| {
            std::fs::write(
                execution.join("state/sdk-run.stdout.txt"),
                b"SDK candidate consumer passed\n",
            )?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!execution.exists());
        Ok(())
    })
}

#[test]
fn preservation_failures_keep_the_process_error_and_do_not_overwrite_existing_artifacts(
) -> Result<(), std::io::Error> {
    candidate("http-preservation-failure", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phase = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phase += 1;
                perform_preparation(&execution, phase, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        std::fs::write(
            artifacts.join("sdk-resolve.stdout.json"),
            b"preserve user data",
        )?;
        let target = SdkHttpTarget::new(
            "127.0.0.1:8088".parse().map_err(std::io::Error::other)?,
            "api",
            "fixture-password",
            "0.1.0",
            "sdk-run",
        )
        .map_err(std::io::Error::other)?;
        let result = prepared.run_with(&target, |_, _| Err(ProcessError::Interrupted));
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected operation and preservation failure"))?
            .to_string();
        assert!(error.contains("interrupted"));
        assert!(error.contains("preservation"));
        assert_eq!(
            std::fs::read(artifacts.join("sdk-resolve.stdout.json"))?,
            b"preserve user data"
        );
        assert!(!execution.exists());
        Ok(())
    })
}

#[test]
fn dropping_a_prepared_consumer_preserves_logs_and_removes_the_owned_copy(
) -> Result<(), std::io::Error> {
    candidate("http-drop", |candidate| {
        let contents = SdkContents::read(candidate).map_err(std::io::Error::other)?;
        let execution = candidate.with_file_name("execution");
        let artifacts = candidate.with_file_name("artifacts");
        let mut phase = 0;
        let prepared = PreparedSdkHttpConsumer::prepare_with(
            &contents,
            &execution,
            &artifacts,
            |command, budget| {
                phase += 1;
                perform_preparation(&execution, phase, command, budget)
            },
        )
        .map_err(std::io::Error::other)?;
        drop(prepared);
        assert!(!execution.exists());
        assert!(artifacts.join("sdk-metadata.stdout.json").is_file());
        Ok(())
    })
}
