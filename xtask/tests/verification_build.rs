use xtask::reference::{ReferenceError, VerificationBuild};

#[test]
fn verification_builds_own_their_builder_and_never_use_a_private_parent(
) -> Result<(), ReferenceError> {
    let plan = VerificationBuild::new("fixture")?;
    let steps = plan.steps();
    assert_eq!(steps.len(), 4);
    assert!(
        steps[0]
            .arguments
            .iter()
            .any(|value| value
                .contains("memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000"))
    );
    assert!(steps[2]
        .arguments
        .iter()
        .any(|value| value == "--pids-limit=256"));
    assert!(steps[3]
        .arguments
        .iter()
        .any(|value| value == "containers/input-verification"));
    assert!(steps[3]
        .arguments
        .iter()
        .any(|value| value == "rusterisk-input-verification:rust-1.99.0"));
    assert_eq!(steps[3].budget.as_secs(), 900);
    assert!(plan
        .cleanup()
        .arguments
        .iter()
        .any(|value| value == "rusterisk-verification-fixture"));
    for invalid in ["", "-shared", "other/name"] {
        assert!(VerificationBuild::new(invalid).is_err());
    }
    Ok(())
}
