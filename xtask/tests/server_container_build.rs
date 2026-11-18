use std::{io, path::Path, time::Duration};
use xtask::server_distribution::{ServerBuildPhase, ServerContainerBuildPlan, ServerTarget};

const IMAGE: &str = "sha256:8ba30003af95ec74b9dff0aaadcb661bcc68d260a50b912c4ac3203d67c7afbb";

#[test]
fn linux_builds_bound_resources_and_separate_fetch_from_offline_compilation() -> io::Result<()> {
    for (target, platform) in [
        (ServerTarget::LinuxArm64, "linux/arm64"),
        (ServerTarget::LinuxX86_64, "linux/amd64"),
    ] {
        let plan = ServerContainerBuildPlan::new(
            "fixture",
            Path::new("/source"),
            Path::new("/state"),
            target,
            IMAGE,
        )?;
        for phase in ServerBuildPhase::ALL {
            let step = plan.step(phase);
            for required in [
                "run",
                "--rm",
                "--cpus=2",
                "--memory=2g",
                "--memory-swap=2g",
                "--pids-limit=256",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                IMAGE,
                platform,
                target.triple(),
                "CARGO_HOME=/state/cargo",
                "CARGO_BUILD_JOBS=2",
                "type=bind,source=/source,target=/source,readonly",
                "type=bind,source=/state,target=/state",
            ] {
                assert!(
                    step.arguments.iter().any(|argument| argument == required),
                    "{required}"
                );
            }
            let offline = match phase {
                ServerBuildPhase::Fetch => "CARGO_NET_OFFLINE=false",
                ServerBuildPhase::Build => "CARGO_NET_OFFLINE=true",
            };
            assert!(step.arguments.iter().any(|argument| argument == offline));
            match phase {
                ServerBuildPhase::Fetch => assert_eq!(step.budget, Duration::from_secs(180)),
                ServerBuildPhase::Build => {
                    assert_eq!(step.budget, Duration::from_secs(900));
                    assert!(step
                        .arguments
                        .iter()
                        .any(|argument| argument == "--network=none"));
                    assert!(step
                        .arguments
                        .windows(2)
                        .any(|pair| pair == ["--target-dir", "/state/target"]));
                    assert!(!step
                        .arguments
                        .iter()
                        .any(|argument| argument == "/state/target/target"));
                }
            }
            let cleanup = plan.cleanup(phase);
            assert_eq!(&cleanup.arguments[..2], &["rm", "--force"]);
            assert_eq!(cleanup.budget, Duration::from_secs(30));
            assert!(cleanup.arguments[2].starts_with("rusterisk-server-build-fixture-"));
        }
    }
    Ok(())
}

#[test]
fn container_builds_reject_foreign_targets_unpinned_images_and_ambiguous_mounts() {
    for target in [
        ServerTarget::MacOsArm64,
        ServerTarget::MacOsX86_64,
        ServerTarget::WindowsX86_64,
    ] {
        assert!(ServerContainerBuildPlan::new(
            "fixture",
            Path::new("/source"),
            Path::new("/state"),
            target,
            IMAGE
        )
        .is_err());
    }
    for (identity, source, state, image) in [
        ("", "/source", "/state", IMAGE),
        ("fixture", "/source,readonly", "/state", IMAGE),
        ("fixture", "/source", "/state,other", IMAGE),
        ("fixture", "relative", "/state", IMAGE),
        ("fixture", "/source", "/source/nested", IMAGE),
        ("fixture", "/source", "/state", "rust:latest"),
    ] {
        assert!(ServerContainerBuildPlan::new(
            identity,
            Path::new(source),
            Path::new(state),
            ServerTarget::LinuxArm64,
            image
        )
        .is_err());
    }
}
