use xtask::reference::{CoreProofBuild, ReferenceError};

#[test]
fn core_proof_builds_use_the_public_context_and_own_their_resource_limits(
) -> Result<(), ReferenceError> {
    let plan = CoreProofBuild::new("fixture")?;
    let steps = plan.steps();
    assert_eq!(steps.len(), 4);
    assert!(
        steps[0]
            .arguments
            .iter()
            .any(|value| value
                .contains("memory=2g,memory-swap=2g,cpu-quota=200000,cpu-period=100000"))
    );
    assert!(steps[0]
        .arguments
        .iter()
        .any(|value| value.contains("image=moby/buildkit@sha256:")));
    assert_eq!(
        steps[2].arguments[..3],
        [
            "update",
            "--pids-limit=256",
            "buildx_buildkit_rusterisk-core-proofs-fixture0"
        ]
    );
    for required in [
        "--builder",
        "rusterisk-core-proofs-fixture",
        "--load",
        "--progress=plain",
        "--tag",
        "rusterisk-core-proofs:kani-0.68.0",
        "containers/core-proofs",
    ] {
        assert!(
            steps[3].arguments.iter().any(|value| value == required),
            "{required}"
        );
    }
    assert_eq!(steps[3].budget.as_secs(), 900);
    assert_eq!(
        plan.cleanup().arguments,
        ["buildx", "rm", "--force", "rusterisk-core-proofs-fixture"]
    );
    assert_eq!(plan.cleanup().budget.as_secs(), 60);
    for invalid in [
        "",
        "-shared",
        "other/name",
        "name with space",
        &"x".repeat(65),
    ] {
        assert!(CoreProofBuild::new(invalid).is_err());
    }
    Ok(())
}
