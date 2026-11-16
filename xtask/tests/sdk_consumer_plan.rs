use std::{path::Path, time::Duration};
use xtask::distribution::{SdkConsumerAction, SdkConsumerPhase, SdkConsumerPlan, SdkConsumerScope};

#[test]
fn independent_consumers_resolve_then_inspect_build_and_run_with_finite_budgets(
) -> Result<(), std::io::Error> {
    let work = std::env::temp_dir().join("rusterisk-sdk-consumer-work");
    let state = std::env::temp_dir().join("rusterisk-sdk-consumer-state");
    let plan = SdkConsumerPlan::new(&work, &state)?;
    assert_eq!(plan.work(), work);
    assert_eq!(plan.state(), state);
    assert_eq!(
        SdkConsumerPhase::ALL.map(SdkConsumerPhase::label),
        [
            "core-msrv-resolve",
            "core-msrv-metadata",
            "core-msrv-build",
            "core-msrv-run",
            "sdk-resolve",
            "sdk-metadata",
            "sdk-build",
            "sdk-run"
        ]
    );
    for scope in SdkConsumerScope::ALL {
        for (action, seconds) in [
            (SdkConsumerAction::Resolve, 180),
            (SdkConsumerAction::Metadata, 60),
            (SdkConsumerAction::Build, 900),
            (SdkConsumerAction::Run, 30),
        ] {
            let phase = SdkConsumerPhase::new(scope, action);
            let step = plan.step(phase);
            assert_eq!(step.budget, Duration::from_secs(seconds));
            assert_eq!(
                plan.directory(scope),
                work.join("consumers").join(scope.label())
            );
            assert_eq!(plan.target(scope), state.join("target").join(scope.label()));
            if action == SdkConsumerAction::Run {
                assert_eq!(
                    plan.program(phase),
                    plan.target(scope)
                        .join("debug")
                        .join(format!(
                            "{}{}",
                            scope.package_name(),
                            std::env::consts::EXE_SUFFIX
                        ))
                        .to_str()
                        .ok_or_else(|| std::io::Error::other("fixture executable is not UTF-8"))?
                );
                assert!(step.arguments.is_empty());
            } else {
                assert!(step
                    .arguments
                    .iter()
                    .any(|argument| argument == "--offline"));
                assert!(!step
                    .arguments
                    .iter()
                    .any(|argument| argument == "--no-deps"));
                if scope == SdkConsumerScope::CoreMsrv {
                    assert_eq!(plan.program(phase), "rustup");
                    assert_eq!(
                        step.arguments.get(..3),
                        Some(["run", "1.89.0", "cargo"].map(str::to_owned).as_slice())
                    );
                } else {
                    assert_eq!(plan.program(phase), "cargo");
                }
            }
        }
    }
    let metadata = plan.step(SdkConsumerPhase::new(
        SdkConsumerScope::Sdk,
        SdkConsumerAction::Metadata,
    ));
    assert_eq!(
        metadata.arguments,
        ["metadata", "--format-version", "1", "--offline", "--locked"]
    );
    assert_eq!(
        plan.step(SdkConsumerPhase::new(
            SdkConsumerScope::Sdk,
            SdkConsumerAction::Resolve
        ))
        .arguments,
        ["generate-lockfile", "--offline"]
    );
    let build = plan.step(SdkConsumerPhase::new(
        SdkConsumerScope::Sdk,
        SdkConsumerAction::Build,
    ));
    assert_eq!(
        build.arguments,
        [
            "build",
            "--locked",
            "--offline",
            "--jobs",
            "2",
            "--target-dir",
            plan.target(SdkConsumerScope::Sdk)
                .to_str()
                .ok_or_else(|| std::io::Error::other("fixture target is not UTF-8"))?
        ]
    );
    Ok(())
}

#[test]
fn consumer_roots_cannot_alias_each_other_or_use_relative_escape_paths() {
    let root = std::env::temp_dir().join("rusterisk-sdk-consumer");
    assert!(SdkConsumerPlan::new(Path::new("relative"), &root).is_err());
    assert!(SdkConsumerPlan::new(&root, Path::new("relative")).is_err());
    assert!(SdkConsumerPlan::new(&root, &root).is_err());
    assert!(SdkConsumerPlan::new(&root, &root.join("state")).is_err());
    assert!(SdkConsumerPlan::new(&root.join("work"), &root).is_err());
    assert!(SdkConsumerPlan::new(&root.join("../escape"), &root).is_err());
}
