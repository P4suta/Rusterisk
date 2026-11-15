use xtask::{browser::Build, reference::ReferenceError};

#[test]
fn browser_builds_isolate_resources_and_remove_only_the_owned_builder() -> Result<(), ReferenceError>
{
    let plan = Build::new("fixture")?;
    let steps = plan.steps();
    assert_eq!(steps.len(), 4);
    assert!(steps[0].arguments.iter().any(|value| value == "memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000,restart-policy=no,image=moby/buildkit@sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe"));
    assert!(!steps[0].arguments.iter().any(|value| value == "--use"));
    assert!(steps[2]
        .arguments
        .iter()
        .any(|value| value == "--pids-limit=256"));
    assert_eq!(steps[3].budget.as_secs(), 900);
    assert!(steps[3].arguments.iter().any(|value| value == "--load"));
    assert!(!steps[3].arguments.iter().any(|value| value == "--push"));
    assert!(steps[3]
        .arguments
        .iter()
        .any(|value| value == "rusterisk-browser-verification:rust-1.99.0-playwright-1.65.1"));
    assert_eq!(
        steps[3].arguments.last().map(String::as_str),
        Some("containers/browser-verification")
    );
    let cleanup = plan.cleanup();
    assert_eq!(cleanup.budget.as_secs(), 60);
    assert_eq!(
        cleanup.arguments,
        ["buildx", "rm", "--force", "rusterisk-browser-fixture"]
    );
    assert_ne!(Build::new("other")?.cleanup().arguments, cleanup.arguments);
    for invalid in [
        "",
        "--all-inactive",
        "../shared",
        "with space",
        &"x".repeat(65),
    ] {
        assert!(Build::new(invalid).is_err());
    }
    Ok(())
}
