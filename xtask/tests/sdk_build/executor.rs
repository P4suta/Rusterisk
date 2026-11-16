use std::path::{Path, PathBuf};
use xtask::distribution::{SdkBuildError, SdkBuildPhase, SdkBuildPlan};

fn fixture(label: &str) -> Result<PathBuf, std::io::Error> {
    let path = std::env::temp_dir().join(format!(
        "rusterisk-sdk-executor-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir(&path)?;
    std::fs::create_dir(path.join("source"))?;
    std::fs::create_dir(path.join("state"))?;
    Ok(path)
}

fn using_fixture(
    label: &str,
    test: impl FnOnce(&Path) -> Result<(), SdkBuildError>,
) -> Result<(), SdkBuildError> {
    let path = fixture(label)?;
    let operation = test(&path);
    let cleanup = std::fs::remove_dir_all(path);
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(operation), Err(cleanup)) => Err(std::io::Error::other(format!(
            "{operation}; fixture cleanup failed: {cleanup}"
        ))
        .into()),
    }
}

#[test]
fn sdk_package_executor_runs_in_the_copy_and_stops_after_the_first_failure(
) -> Result<(), SdkBuildError> {
    using_fixture("failure", |root| {
        let plan = SdkBuildPlan::new(&root.join("source"), &root.join("state"))?;
        let mut calls = Vec::new();
        let result = plan.execute_with(|command, budget| {
            calls.push((
                command.get_current_dir().map(Path::to_owned),
                command
                    .get_args()
                    .map(|value| value.to_owned())
                    .collect::<Vec<_>>(),
                budget,
            ));
            Err(xtask::process::ProcessError::Timeout)
        });
        assert!(matches!(
            result,
            Err(SdkBuildError::Process(
                xtask::process::ProcessError::Timeout
            ))
        ));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_deref(), Some(root.join("source").as_path()));
        assert_eq!(calls[0].1, ["fetch", "--locked"]);
        assert_eq!(calls[0].2, plan.step(SdkBuildPhase::Fetch).budget);
        assert!(root.join("state/fetch.txt").is_file());
        assert!(!root.join("state/package.txt").exists());
        Ok(())
    })
}

#[test]
fn sdk_package_executor_refuses_to_overwrite_an_existing_phase_log() -> Result<(), SdkBuildError> {
    using_fixture("logs", |root| {
        std::fs::write(root.join("state/fetch.txt"), b"preserved")?;
        let plan = SdkBuildPlan::new(&root.join("source"), &root.join("state"))?;
        let mut invoked = false;
        assert!(plan
            .execute_with(|_, _| {
                invoked = true;
                Ok(())
            })
            .is_err());
        assert!(!invoked);
        assert_eq!(std::fs::read(root.join("state/fetch.txt"))?, b"preserved");
        Ok(())
    })
}

#[test]
fn sdk_package_executor_enables_downloads_only_for_fetching() -> Result<(), SdkBuildError> {
    using_fixture("success", |root| {
        let plan = SdkBuildPlan::new(&root.join("source"), &root.join("state"))?;
        let mut phases = Vec::new();
        plan.execute_with(|command, _| {
            assert_eq!(command.get_program(), "cargo");
            let phase = command
                .get_args()
                .next()
                .and_then(|value| value.to_str())
                .ok_or(xtask::process::ProcessError::InvalidOutput)?;
            let offline = command
                .get_envs()
                .find(|(name, _)| *name == "CARGO_NET_OFFLINE")
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str());
            assert_eq!(
                offline,
                Some(if phase == "fetch" { "false" } else { "true" })
            );
            phases.push(phase.to_owned());
            Ok(())
        })?;
        assert_eq!(phases, ["fetch", "package"]);
        assert!(root.join("state/package.txt").is_file());
        Ok(())
    })
}
