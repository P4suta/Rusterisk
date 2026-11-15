use xtask::reference::{ReferenceBuild, ReferenceError};

#[test]
fn reference_builds_isolate_resources_and_cleanup_without_selecting_a_shared_builder(
) -> Result<(), ReferenceError> {
    let plan = ReferenceBuild::new("fixture")?;
    let steps = plan.steps();
    assert_eq!(steps.len(), 4);
    let creation = &steps[0].arguments;
    assert!(creation.iter().any(|value| value == "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000,restart-policy=no,image=moby/buildkit@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe"));
    assert!(!creation.iter().any(|value| value == "--use"));
    assert!(steps[2]
        .arguments
        .iter()
        .any(|value| value == "--pids-limit=256"));
    assert_eq!(steps[3].budget.as_secs(), 1800);
    assert!(steps[3].arguments.iter().any(|value| value == "--load"));
    assert!(!steps[3].arguments.iter().any(|value| value == "--push"));
    let cleanup = plan.cleanup();
    assert_eq!(
        cleanup.arguments,
        ["buildx", "rm", "--force", "rusterisk-reference-fixture"]
    );
    assert!(ReferenceBuild::new("other")?.cleanup().arguments != cleanup.arguments);
    for invalid in [
        "",
        "--all-inactive",
        "../shared",
        "with space",
        &"x".repeat(65),
    ] {
        assert!(ReferenceBuild::new(invalid).is_err());
    }
    Ok(())
}
