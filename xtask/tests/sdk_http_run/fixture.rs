use std::{path::Path, process::Command, time::Duration};
use xtask::{
    distribution::{SdkConsumerPlan, SdkConsumerScope, SdkRuntimeIdentity},
    process::ProcessError,
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
    command: &mut Command,
    budget: Duration,
) -> Result<(), ProcessError> {
    let plan = SdkConsumerPlan::new(&execution.join("work"), &execution.join("state"))?;
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
        3 => assert_eq!(budget, Duration::from_secs(900)),
        _ => return Err(std::io::Error::other("unexpected preparation phase").into()),
    }
    Ok(())
}
