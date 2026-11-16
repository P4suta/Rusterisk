use std::{io, path::Path, time::Duration};
use xtask::server_distribution::{
    ServerOciBuildPhase, ServerOciBuildPlan, ServerOciProfile, ServerTarget,
};

#[test]
fn image_builds_export_candidate_bytes_with_an_owned_bounded_builder() -> io::Result<()> {
    let profile = ServerOciProfile::new(ServerTarget::LinuxArm64, "0.1.0", &"a".repeat(64))?;
    let plan = ServerOciBuildPlan::new(
        "fixture",
        Path::new("/context"),
        Path::new("/artifacts/server.oci.tar"),
        &profile,
    )?;
    let create = plan.step(ServerOciBuildPhase::CreateBuilder);
    assert_eq!(create.arguments[0], "buildx");
    assert!(create
        .arguments
        .iter()
        .any(|value| value.contains("memory=2g,memory-swap=2g,cpu-quota=200000")));
    assert!(create
        .arguments
        .iter()
        .any(|value| value.contains("image=moby/buildkit@sha256:")));
    let bound = plan.step(ServerOciBuildPhase::BoundProcesses);
    assert!(bound
        .arguments
        .iter()
        .any(|value| value == "--pids-limit=256"));
    let export = plan.step(ServerOciBuildPhase::Export);
    assert_eq!(export.budget, Duration::from_secs(300));
    for value in [
        "buildx",
        "build",
        "--platform=linux/arm64",
        "--network=none",
        "SOURCE_DATE_EPOCH=0",
        "/context",
    ] {
        assert!(
            export.arguments.iter().any(|argument| argument == value),
            "{value}"
        );
    }
    assert!(export
        .arguments
        .iter()
        .any(|value| value.starts_with("type=oci,dest=/artifacts/server.oci.tar,")));
    assert!(!export
        .arguments
        .iter()
        .any(|value| value == "--push" || value == "--load"));
    let cleanup = plan.cleanup();
    assert_eq!(&cleanup.arguments[..3], &["buildx", "rm", "--force"]);
    assert_eq!(cleanup.budget, Duration::from_secs(60));
    Ok(())
}

#[test]
fn image_builds_refuse_unowned_or_ambiguous_output_paths() -> io::Result<()> {
    let profile = ServerOciProfile::new(ServerTarget::LinuxArm64, "0.1.0", &"a".repeat(64))?;
    for (identity, context, output) in [
        ("", "/context", "/artifacts/server.oci.tar"),
        ("fixture", "relative", "/artifacts/server.oci.tar"),
        ("fixture", "/context", "relative"),
        ("fixture", "/context", "/context/server.oci.tar"),
        ("fixture", "/context", "/artifacts/image,other.tar"),
        ("fixture", "/context", "/artifacts/../server.oci.tar"),
    ] {
        assert!(
            ServerOciBuildPlan::new(identity, Path::new(context), Path::new(output), &profile)
                .is_err()
        );
    }
    Ok(())
}
