use std::{num::NonZeroUsize, path::Path, time::Duration};
use xtask::distribution::{SdkConsumerAction, SdkConsumerPhase, SdkConsumerPlan, SdkConsumerScope};

#[cfg(windows)]
#[test]
fn sdk_cargo_targets_use_ordinary_drive_paths() -> Result<(), std::io::Error> {
    let work = Path::new(r"\\?\C:\fixture\work");
    let state = Path::new(r"\\?\C:\fixture\state");
    let plan = SdkConsumerPlan::new(work, state)?;
    for scope in SdkConsumerScope::ALL {
        let target = format!(r"C:\fixture\state\target\{}", scope.label());
        assert_eq!(plan.target(scope), state.join("target").join(scope.label()));
        let step = plan.step(SdkConsumerPhase::new(scope, SdkConsumerAction::Build));
        assert_eq!(step.arguments.last(), Some(&target));
    }
    Ok(())
}

#[test]
fn independent_consumers_resolve_then_inspect_build_and_run_with_finite_budgets(
) -> Result<(), std::io::Error> {
    let work = std::env::temp_dir().join("rusterisk-sdk-consumer-work");
    let state = std::env::temp_dir().join("rusterisk-sdk-consumer-state");
    let plan = SdkConsumerPlan::new(&work, &state)?;
    assert_eq!(plan.jobs(), SdkConsumerPlan::DEFAULT_JOBS);
    assert_eq!(plan.jobs().get(), 2);
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
        ["metadata", "--format-version", "1", "--offline"]
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
fn explicit_jobs_change_only_consumer_build_parallelism() -> std::io::Result<()> {
    let work = std::env::temp_dir().join("rusterisk-sdk-single-job-work");
    let state = std::env::temp_dir().join("rusterisk-sdk-single-job-state");
    let default = SdkConsumerPlan::new(&work, &state)?;
    let selected = SdkConsumerPlan::new_with_jobs(&work, &state, NonZeroUsize::MIN)?;
    assert_eq!(selected.jobs(), NonZeroUsize::MIN);
    for phase in SdkConsumerPhase::ALL {
        let step = selected.step(phase);
        let baseline = default.step(phase);
        assert_eq!(selected.program(phase), default.program(phase));
        assert_eq!(step.budget, baseline.budget);
        assert_eq!(
            selected.directory(phase.scope()),
            default.directory(phase.scope())
        );
        assert_eq!(
            selected.target(phase.scope()),
            default.target(phase.scope())
        );
        if phase.action() == SdkConsumerAction::Build {
            let arguments: Vec<_> = step.arguments.iter().map(String::as_str).collect();
            let cargo = if phase.scope() == SdkConsumerScope::CoreMsrv {
                assert_eq!(
                    arguments.get(..3),
                    Some(["run", "1.89.0", "cargo"].as_slice())
                );
                arguments.get(3..)
            } else {
                Some(arguments.as_slice())
            }
            .ok_or_else(|| std::io::Error::other("consumer build command is incomplete"))?;
            assert_eq!(
                cargo,
                [
                    "build",
                    "--locked",
                    "--offline",
                    "--jobs",
                    "1",
                    "--target-dir",
                    selected.target(phase.scope()).to_str().ok_or_else(|| {
                        std::io::Error::other("fixture target directory is not UTF-8")
                    })?
                ]
            );
            assert_eq!(step.budget, Duration::from_secs(900));
        } else {
            assert_eq!(step.arguments, baseline.arguments);
        }
    }
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
