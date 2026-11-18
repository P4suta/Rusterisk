use std::{num::NonZeroUsize, path::Path, time::Duration};
use xtask::{
    distribution::{SdkConsumerPlan, SdkConsumerScope, SdkRuntimeIdentity},
    process::{ProcessError, SupervisedCommand},
};

pub fn candidate(
    label: &str,
    test: impl FnOnce(&Path) -> Result<(), std::io::Error>,
) -> Result<(), std::io::Error> {
    crate::candidate_fixture::using_candidate(
        label,
        |fixtures| {
            let lock = fixtures[2]
                .files
                .get_mut("Cargo.lock")
                .ok_or_else(|| std::io::Error::other("fixture client lock missing"))?;
            lock.extend_from_slice(format!("[[package]]\nname='tokio'\nversion='1.54.1'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{}'\n", "a".repeat(64)).as_bytes());
            Ok(())
        },
        test,
    )
}

pub fn perform_preparation(
    execution: &Path,
    phase: usize,
    command: SupervisedCommand,
    budget: Duration,
) -> Result<(), ProcessError> {
    perform_preparation_with_jobs(
        execution,
        phase,
        command,
        budget,
        SdkConsumerPlan::DEFAULT_JOBS,
    )
}

pub fn perform_preparation_with_jobs(
    execution: &Path,
    phase: usize,
    command: SupervisedCommand,
    budget: Duration,
    jobs: NonZeroUsize,
) -> Result<(), ProcessError> {
    let plan = SdkConsumerPlan::new(&execution.join("work"), &execution.join("state"))?;
    let command = command.into_std();
    let jobs = jobs.to_string();
    assert_eq!(
        command
            .get_envs()
            .find(|(name, _)| *name == "CARGO_BUILD_JOBS")
            .and_then(|(_, value)| value),
        Some(std::ffi::OsStr::new(&jobs))
    );
    assert!(!command
        .get_envs()
        .any(|(name, _)| name.to_string_lossy().starts_with("RUSTERISK_SDK_HTTP_")));
    assert_eq!(command.get_program(), "cargo");
    assert_eq!(
        command.get_current_dir(),
        Some(plan.directory(SdkConsumerScope::Sdk))
    );
    match phase {
        1 => {
            assert_eq!(budget, Duration::from_secs(180));
            let mut lock = "version=4\n".to_owned();
            for (name, version) in [
                ("rusterisk-core", "0.1.0"),
                ("rusterisk-api", "0.1.0"),
                ("rusterisk-client", "0.1.0"),
                ("rusterisk-sdk-candidate-consumer", "0.0.0"),
            ] {
                lock.push_str(&format!(
                    "[[package]]\nname='{name}'\nversion='{version}'\n"
                ));
            }
            lock.push_str(&format!("[[package]]\nname='tokio'\nversion='1.54.1'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{}'\n", "a".repeat(64)));
            std::fs::write(
                plan.directory(SdkConsumerScope::Sdk).join("Cargo.lock"),
                lock,
            )?;
        }
        2 => {
            assert_eq!(budget, Duration::from_secs(60));
            let seed = std::fs::read(
                plan.work()
                    .join("packages/rusterisk-client-0.1.0/Cargo.lock"),
            )?;
            let runtime = SdkRuntimeIdentity::from_lock(&seed).map_err(std::io::Error::other)?;
            let data = crate::http_fixture::with_runtime(&plan, &runtime)?;
            std::fs::write(
                plan.state().join("sdk-metadata.stdout.json"),
                crate::http_fixture::bytes(&data, 0)?,
            )?;
        }
        3 => {
            assert_eq!(budget, Duration::from_secs(900));
            let arguments: Vec<_> = command.get_args().collect();
            let selected: Vec<_> = arguments
                .windows(2)
                .filter(|pair| pair[0] == "--jobs")
                .map(|pair| pair[1])
                .collect();
            assert_eq!(selected, [std::ffi::OsStr::new(&jobs)]);
        }
        _ => return Err(std::io::Error::other("unexpected preparation phase").into()),
    }
    Ok(())
}
