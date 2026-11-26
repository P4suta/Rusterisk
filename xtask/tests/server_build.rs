use std::{path::Path, time::Duration};
use xtask::server_distribution::{
    NativeServerBuildLimits, ServerBuildPhase, ServerBuildPlan, ServerTarget,
};

#[cfg(windows)]
#[test]
fn windows_cargo_uses_ordinary_drive_paths_for_owned_targets() -> std::io::Result<()> {
    let source = Path::new(r"\\?\C:\capture\source");
    let state = Path::new(r"\\?\C:\capture\state");
    let plan = ServerBuildPlan::new(source, state, ServerTarget::WindowsX86_64)?;
    let build = plan.step(ServerBuildPhase::Build);
    assert_eq!(
        build.arguments.last().map(String::as_str),
        Some(r"C:\capture\state\target")
    );
    assert_eq!(plan.state(), state);
    assert!(plan.executable().starts_with(state));
    Ok(())
}

#[test]
fn explicit_native_build_limits_change_only_jobs_and_the_build_deadline() -> std::io::Result<()> {
    let source = std::env::temp_dir().join("rusterisk-server-limited-source");
    let state = std::env::temp_dir().join("rusterisk-server-limited-state");
    let limits = NativeServerBuildLimits::new(1, 1800)?;
    for target in ServerTarget::ALL {
        let default = ServerBuildPlan::new(&source, &state, target)?;
        let explicit = ServerBuildPlan::new_with_limits(&source, &state, target, limits)?;
        let fetch = explicit.step(ServerBuildPhase::Fetch);
        assert_eq!(
            fetch.arguments,
            default.step(ServerBuildPhase::Fetch).arguments
        );
        assert_eq!(fetch.budget, Duration::from_secs(180));
        let build = explicit.step(ServerBuildPhase::Build);
        let mut expected = default.step(ServerBuildPhase::Build).arguments;
        let jobs = expected
            .windows(2)
            .position(|pair| pair[0] == "--jobs")
            .ok_or_else(|| std::io::Error::other("default build jobs missing"))?;
        expected[jobs + 1] = "1".to_owned();
        assert_eq!(build.arguments, expected);
        assert_eq!(build.budget, Duration::from_secs(1800));
        assert_eq!(explicit.executable(), default.executable());
    }
    Ok(())
}

#[test]
fn native_build_limits_require_positive_jobs_and_a_positive_bounded_timeout() -> std::io::Result<()>
{
    for (jobs, seconds) in [(0, 1800), (1, 0), (0, 0)] {
        assert!(NativeServerBuildLimits::new(jobs, seconds).is_err());
    }
    NativeServerBuildLimits::new(1, u16::MAX)?;
    Ok(())
}

#[test]
fn server_builds_select_both_release_executables_with_locked_bounded_steps() -> std::io::Result<()>
{
    let source = std::env::temp_dir().join("rusterisk-server-build-source");
    let state = std::env::temp_dir().join("rusterisk-server-build-state");
    for target in ServerTarget::ALL {
        let plan = ServerBuildPlan::new(&source, &state, target)?;
        let fetch = plan.step(ServerBuildPhase::Fetch);
        assert_eq!(
            fetch.arguments,
            ["fetch", "--locked", "--target", target.triple()]
        );
        assert_eq!(fetch.budget, Duration::from_secs(180));
        let build = plan.step(ServerBuildPhase::Build);
        let target_dir = state.join("target");
        assert_eq!(
            build.arguments,
            [
                "build",
                "--locked",
                "--offline",
                "--release",
                "--package",
                "rusterisk-server",
                "--package",
                "rusterisk-cli",
                "--bin",
                "rusterisk",
                "--bin",
                "rusterisk-cli",
                "--target",
                target.triple(),
                "--jobs",
                "2",
                "--target-dir",
                target_dir
                    .to_str()
                    .ok_or_else(|| std::io::Error::other("fixture path is not UTF-8"))?,
            ]
        );
        assert_eq!(build.budget, Duration::from_secs(900));
        assert_eq!(plan.source(), source);
        assert_eq!(plan.state(), state);
        assert_eq!(plan.target(), target);
        assert_eq!(
            plan.executable(),
            target_dir.join(target.triple()).join("release").join(
                if target == ServerTarget::WindowsX86_64 {
                    "rusterisk.exe"
                } else {
                    "rusterisk"
                }
            )
        );
        assert_eq!(
            ServerBuildPhase::ALL,
            [ServerBuildPhase::Fetch, ServerBuildPhase::Build]
        );
    }
    Ok(())
}

#[test]
fn server_build_paths_cannot_overlap_escape_or_embed_control_characters() {
    let root = std::env::temp_dir().join("rusterisk-server-build");
    for (source, state) in [
        (Path::new("relative").to_owned(), root.clone()),
        (root.clone(), Path::new("relative").to_owned()),
        (root.clone(), root.clone()),
        (root.clone(), root.join("nested")),
        (root.join("nested"), root.clone()),
        (root.join("../source"), root.join("state")),
        (root.join("line\nsource"), root.join("state")),
    ] {
        assert!(ServerBuildPlan::new(&source, &state, ServerTarget::MacOsArm64).is_err());
    }
}
