use std::{path::Path, time::Duration};
use xtask::server_distribution::{ServerBuildPhase, ServerBuildPlan, ServerTarget};

#[test]
fn server_builds_select_one_release_executable_with_locked_bounded_steps() -> std::io::Result<()> {
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
                "--bin",
                "rusterisk",
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
