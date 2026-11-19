use super::{complete, CoreProofPlan, ProofError, HARNESSES, IMAGE};

fn report() -> serde_json::Value {
    let mut report = complete();
    report["cbmc"] = HARNESSES
        .into_iter()
        .map(|name| serde_json::json!({"harness_id": name, "configuration": {"solver": "kissat"}}))
        .collect();
    report
}

#[test]
fn disk_scratch_profile_preserves_the_bounded_complete_proof_run() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    let run = plan.run();
    assert!(run
        .arguments
        .windows(2)
        .any(|pair| { pair == ["--mount", "type=volume,target=/tmp,volume-nocopy"] }));
    assert!(!run.arguments.iter().any(|value| value == "--tmpfs"));
    for required in [
        "--rm",
        "--read-only",
        "--cpus=2",
        "--memory=2g",
        "--memory-swap=2g",
        "--env=TMPDIR=/tmp",
        "--target-dir=/tmp/target",
        "--env=CARGO_HOME=/tmp/cargo",
        "--solver=kissat",
        "--jobs=1",
        "--harness-timeout=120s",
        "--export-json=/results/kani.json",
    ] {
        assert!(
            run.arguments.iter().any(|value| value == required),
            "{required}"
        );
    }
    let selected: Vec<_> = run
        .arguments
        .windows(2)
        .filter(|pair| pair[0] == "--harness")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(selected, HARNESSES);
    assert_eq!(run.budget.as_secs(), 900);
    assert_eq!(
        plan.cleanup().arguments,
        [
            "rm",
            "--force",
            "--volumes",
            "rusterisk-core-proofs-fixture"
        ]
    );
    Ok(())
}

#[test]
fn solver_profile_accepts_native_configuration_for_every_harness() -> Result<(), ProofError> {
    super::validate_results(&serde_json::to_vec(&report()).map_err(ProofError::Json)?)
}

#[test]
fn solver_profile_rejects_incomplete_foreign_or_different_native_configuration(
) -> Result<(), ProofError> {
    let mut cases = Vec::new();
    let mut missing = report();
    missing
        .as_object_mut()
        .ok_or(ProofError::Results)?
        .remove("cbmc")
        .ok_or(ProofError::Results)?;
    cases.push(missing);
    for solver in [serde_json::json!("cadical"), serde_json::Value::Null] {
        let mut changed = report();
        changed["cbmc"][0]["configuration"]["solver"] = solver;
        cases.push(changed);
    }
    let mut absent = report();
    absent["cbmc"]
        .as_array_mut()
        .ok_or(ProofError::Results)?
        .pop()
        .ok_or(ProofError::Results)?;
    cases.push(absent);
    let mut repeated = report();
    repeated["cbmc"][1]["harness_id"] = HARNESSES[0].into();
    cases.push(repeated);
    let mut foreign = report();
    foreign["cbmc"][0]["harness_id"] = "proofs::foreign".into();
    cases.push(foreign);
    for version in [serde_json::json!("4.0.0"), serde_json::Value::Null] {
        let mut changed = report();
        changed["tools"]["solvers"][0]["version"] = version;
        cases.push(changed);
    }
    for rejected in cases {
        assert!(
            super::validate_results(&serde_json::to_vec(&rejected).map_err(ProofError::Json)?)
                .is_err()
        );
    }
    Ok(())
}
