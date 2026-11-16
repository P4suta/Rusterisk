use xtask::core_proofs::{
    validate_harnesses, validate_manifest, validate_results, CoreProofPlan, ProofError, HARNESSES,
};

const IMAGE: &str = "registry.example/core-proofs@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const TRANSFER_HARNESSES: [&str; 3] = [
    "proofs::transfer_completion_is_absorbing",
    "proofs::transfer_observations_stay_in_their_generation",
    "proofs::transfer_detachment_preserves_live_calls",
];
const ATTENDED_HARNESSES: [&str; 3] = [
    "proofs::attended_handoff_preserves_calls_and_bridge_ownership",
    "proofs::attended_progress_stays_in_the_current_generation",
    "proofs::attended_completion_is_absorbing",
];
const PAIR_HARNESSES: [&str; 2] = [
    "proofs::owned_pair_departures_stay_with_the_current_bridge_and_generations",
    "proofs::owned_pair_release_is_absorbing",
];

#[test]
fn the_native_plan_selects_every_owned_pair_contract() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    for harness in PAIR_HARNESSES {
        assert!(
            plan.run()
                .arguments
                .iter()
                .any(|argument| argument == harness),
            "{harness}"
        );
    }
    Ok(())
}

#[test]
fn the_native_plan_selects_every_attended_transfer_contract() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    for harness in ATTENDED_HARNESSES {
        assert!(
            plan.run()
                .arguments
                .iter()
                .any(|argument| argument == harness),
            "{harness}",
        );
    }
    Ok(())
}

#[test]
fn the_native_plan_selects_every_transfer_contract() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    for harness in TRANSFER_HARNESSES {
        assert!(
            plan.run()
                .arguments
                .iter()
                .any(|argument| argument == harness),
            "{harness}"
        );
    }
    Ok(())
}

#[test]
fn core_proofs_pin_the_source_image_scope_and_resource_limits() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    let run = plan.run();
    assert!(!run
        .arguments
        .iter()
        .any(|value| matches!(value.as_str(), "--locked" | "--offline")));
    for argument in [
        "--pull=never",
        "--cpus=2",
        "--memory=2g",
        "--memory-swap=2g",
        "--pids-limit=256",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--env=CARGO_HOME=/tmp/cargo",
        "--ulimit",
        "fsize=134217728:134217728",
        "--jobs=1",
        "--harness-timeout=120s",
        "--exact",
        "/project/proofs/Cargo.toml",
        "--target-dir=/tmp/target",
        "--export-json=/results/kani.json",
    ] {
        assert!(
            run.arguments.iter().any(|value| value == argument),
            "{argument}"
        );
    }
    assert!(run
        .arguments
        .iter()
        .any(|value| value == "type=bind,source=/project/core,target=/project,readonly"));
    assert!(run.arguments.iter().any(|value| value == IMAGE));
    assert_eq!(run.budget.as_secs(), 900);
    assert_eq!(
        run.arguments
            .iter()
            .filter(|value| value.as_str() == "--harness")
            .count(),
        HARNESSES.len()
    );
    for harness in HARNESSES {
        assert!(run.arguments.iter().any(|value| value == harness));
        assert!(harness.starts_with("proofs::"));
    }
    assert_eq!(
        plan.cleanup().arguments,
        ["rm", "--force", "rusterisk-core-proofs-fixture"]
    );
    for image in ["registry.example/core-proofs:latest", "@sha256:0123", "registry.example/core-proofs@sha256:invalid", "registry.example/core-proofs@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg"] {
        assert!(CoreProofPlan::new("fixture", "/project/core", "/project/results", image).is_err());
    }
    assert!(CoreProofPlan::new("../escape", "/project/core", "/project/results", IMAGE).is_err());
    assert!(CoreProofPlan::new("fixture", "/project,core", "/project/results", IMAGE).is_err());
    Ok(())
}

fn complete() -> serde_json::Value {
    let results: Vec<serde_json::Value> = HARNESSES
        .iter()
        .map(|name| {
            let checks: Vec<serde_json::Value> = [
                serde_json::json!({"status": "Success", "category": "assertion"}),
                serde_json::json!({"status": "Satisfied", "category": "cover"}),
            ]
            .into_iter()
            .collect();
            serde_json::json!({"harness_id": name, "status": "Success", "checks": checks})
        })
        .collect();
    serde_json::json!({"metadata":{"version":"1.0", "kani_version":"0.68.0"}, "tools":{"kani":"0.68.0", "cbmc":"6.11.0"}, "verification_results":{"summary":{"total_harnesses":HARNESSES.len(), "executed":HARNESSES.len(), "successful":HARNESSES.len(), "failed":0, "status":"completed"}, "results":results}})
}

#[test]
fn results_without_the_attended_contracts_are_rejected() -> Result<(), ProofError> {
    let mut prior = complete();
    let results = prior["verification_results"]["results"]
        .as_array_mut()
        .ok_or(ProofError::Results)?;
    results.retain(|result| {
        result["harness_id"].as_str().is_some_and(|name| {
            !ATTENDED_HARNESSES.contains(&name) && !PAIR_HARNESSES.contains(&name)
        })
    });
    let count = results.len();
    for field in ["total_harnesses", "executed", "successful"] {
        prior["verification_results"]["summary"][field] = serde_json::json!(count);
    }
    assert_eq!(count, 9);
    assert!(validate_results(&serde_json::to_vec(&prior).map_err(ProofError::Json)?).is_err());
    Ok(())
}

#[test]
fn results_without_owned_pair_contracts_are_rejected() -> Result<(), ProofError> {
    let mut prior = complete();
    let results = prior["verification_results"]["results"]
        .as_array_mut()
        .ok_or(ProofError::Results)?;
    results.retain(|result| {
        result["harness_id"]
            .as_str()
            .is_some_and(|name| !PAIR_HARNESSES.contains(&name))
    });
    let count = results.len();
    for field in ["total_harnesses", "executed", "successful"] {
        prior["verification_results"]["summary"][field] = serde_json::json!(count);
    }
    assert_eq!(count, 12);
    assert!(validate_results(&serde_json::to_vec(&prior).map_err(ProofError::Json)?).is_err());
    Ok(())
}

#[test]
fn results_for_only_the_original_six_contracts_are_rejected() -> Result<(), ProofError> {
    let mut old = complete();
    let results = old["verification_results"]["results"]
        .as_array_mut()
        .ok_or(ProofError::Results)?;
    results.retain(|result| {
        result["harness_id"].as_str().is_some_and(|name| {
            !TRANSFER_HARNESSES.contains(&name)
                && !ATTENDED_HARNESSES.contains(&name)
                && !PAIR_HARNESSES.contains(&name)
        })
    });
    let count = results.len();
    for field in ["total_harnesses", "executed", "successful"] {
        old["verification_results"]["summary"][field] = serde_json::json!(count);
    }
    assert_eq!(count, 6);
    assert!(validate_results(&serde_json::to_vec(&old).map_err(ProofError::Json)?).is_err());
    Ok(())
}

#[test]
fn only_complete_current_version_results_with_reachable_contracts_pass() -> Result<(), ProofError> {
    let complete = complete();
    validate_results(&serde_json::to_vec(&complete).map_err(ProofError::Json)?)?;
    for (pointer, value) in [
        ("/metadata/kani_version", serde_json::json!("0.67.0")),
        ("/tools/cbmc", serde_json::json!("6.10.0")),
        (
            "/verification_results/summary/total_harnesses",
            serde_json::json!(0),
        ),
        (
            "/verification_results/summary/executed",
            serde_json::json!(0),
        ),
        (
            "/verification_results/summary/status",
            serde_json::json!("completed_with_fail_fast"),
        ),
        (
            "/verification_results/results/0/status",
            serde_json::json!("Failure"),
        ),
        (
            "/verification_results/results/0/checks/0/status",
            serde_json::json!("Undetermined"),
        ),
        (
            "/verification_results/results/0/checks/1/status",
            serde_json::json!("Unsatisfiable"),
        ),
        (
            "/verification_results/results/0/harness_id",
            serde_json::json!("unexpected"),
        ),
        (
            "/verification_results/results/0/checks",
            serde_json::json!([]),
        ),
    ] {
        let mut rejected = complete.clone();
        *rejected.pointer_mut(pointer).ok_or(ProofError::Results)? = value;
        assert!(
            validate_results(&serde_json::to_vec(&rejected).map_err(ProofError::Json)?).is_err(),
            "{pointer}"
        );
    }
    assert!(validate_results(b"{}").is_err());
    let mut unreachable = complete.clone();
    unreachable["verification_results"]["results"][0]["checks"]
        .as_array_mut()
        .ok_or(ProofError::Results)?
        .push(serde_json::json!({"status":"Unreachable", "category":"cover"}));
    assert!(
        validate_results(&serde_json::to_vec(&unreachable).map_err(ProofError::Json)?).is_err()
    );
    Ok(())
}

#[test]
fn the_actual_core_harnesses_declare_bounds_assertions_and_cover_goals() -> Result<(), ProofError> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core/src/proofs.rs"),
    )?;
    validate_harnesses(&source)?;
    for rejected in [source.replace("#[kani::unwind(12)]", ""), source.replace("#[kani::proof]", ""), source.replace("kani::cover!", "ignored_cover!"), format!("{source}\nfn excluded() {{ kani::assume(false); }}"), format!("{source}\n#[kani::proof]\n#[kani::unwind(12)]\nfn unexpected() {{ assert!(true); kani::cover!(true); }}")] {
        assert!(validate_harnesses(&rejected).is_err());
    }
    Ok(())
}

#[test]
fn the_proof_manifest_must_compile_the_dependency_free_production_core() -> Result<(), ProofError> {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core/proofs/Cargo.toml"),
    )?;
    validate_manifest(&manifest)?;
    for rejected in [
        manifest.replace("../src/lib.rs", "../src/model.rs"),
        manifest.replace("rusterisk_core", "proof_model"),
        format!("{manifest}\n[dependencies]\nmodel = \"1\"\n"),
    ] {
        assert!(validate_manifest(&rejected).is_err());
    }
    Ok(())
}
