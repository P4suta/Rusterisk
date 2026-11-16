use std::{path::Path, time::Duration};
use xtask::distribution::{SdkBuildPhase, SdkBuildPlan};

#[test]
fn sdk_packaging_verifies_three_candidates_together_in_an_owned_target_directory(
) -> Result<(), xtask::distribution::SdkBuildError> {
    let source = std::env::temp_dir().join("rusterisk-sdk-plan-source");
    let state = std::env::temp_dir().join("rusterisk-sdk-plan-state");
    let plan = SdkBuildPlan::new(&source, &state)?;
    let package = plan.step(SdkBuildPhase::Package);
    let arguments: Vec<_> = package.arguments.iter().map(String::as_str).collect();
    assert_eq!(
        &arguments[..13],
        [
            "package",
            "--locked",
            "--offline",
            "--jobs",
            "2",
            "--package",
            "rusterisk-core",
            "--package",
            "rusterisk-api",
            "--package",
            "rusterisk-client",
            "--target-dir",
            state.join("target").to_str().ok_or_else(|| {
                std::io::Error::other("fixture target directory is not UTF-8")
            })?,
        ]
    );
    assert_eq!(arguments.len(), 13);
    for forbidden in [
        "--no-verify",
        "--allow-dirty",
        "--no-metadata",
        "--workspace",
    ] {
        assert!(!arguments.contains(&forbidden));
    }
    assert_eq!(package.budget, Duration::from_secs(900));
    assert_eq!(plan.source(), source);
    for phase in SdkBuildPhase::ALL {
        let step = plan.step(phase);
        assert!(!step.budget.is_zero() && step.budget <= Duration::from_secs(900));
    }
    assert_eq!(
        plan.step(SdkBuildPhase::Fetch).arguments,
        ["fetch", "--locked"]
    );
    Ok(())
}

#[test]
fn sdk_build_requires_distinct_absolute_source_and_state_paths() {
    let absolute = std::env::temp_dir().join("rusterisk-sdk-plan");
    assert!(SdkBuildPlan::new(Path::new("relative"), &absolute).is_err());
    assert!(SdkBuildPlan::new(&absolute, Path::new("relative")).is_err());
    assert!(SdkBuildPlan::new(&absolute, &absolute).is_err());
    assert!(SdkBuildPlan::new(&absolute, &absolute.join("state")).is_err());
    assert!(SdkBuildPlan::new(&absolute.join("source"), &absolute).is_err());
    assert!(SdkBuildPlan::new(&absolute.join("../source"), &absolute).is_err());
}
