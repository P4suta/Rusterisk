use xtask::core_assurance::{
    normalize_manifest, validate_miri_discovery, validate_miri_execution,
    validate_mutation_inventory, validate_mutation_outcomes, AssuranceError, CoreAssurancePlan,
    Phase,
};

const IMAGE: &str = "registry.example/core-assurance@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn core_assurance_phases_bound_resources_and_own_cleanup() -> Result<(), AssuranceError> {
    let plan = CoreAssurancePlan::new("fixture", "/core", "/state", IMAGE)?;
    for phase in [
        Phase::MiriSetup,
        Phase::MiriInventory,
        Phase::MiriTests,
        Phase::MutationInventory,
        Phase::Mutations,
    ] {
        let step = plan.step(phase);
        for required in [
            "--rm",
            "--pull=never",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=256",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            IMAGE,
        ] {
            assert!(
                step.arguments.iter().any(|argument| argument == required),
                "{required}"
            );
        }
        assert!(step.budget.as_secs() <= 900);
        assert!(plan
            .cleanup(phase)
            .arguments
            .iter()
            .any(|argument| argument.starts_with("rusterisk-core-assurance-fixture-")));
    }
    for phase in [
        Phase::MiriInventory,
        Phase::MiriTests,
        Phase::MutationInventory,
        Phase::Mutations,
    ] {
        assert!(plan
            .step(phase)
            .arguments
            .iter()
            .any(|argument| argument == "--network=none"));
    }
    let tests = plan.step(Phase::MiriTests);
    for required in [
        "+nightly-2026-08-21",
        "miri",
        "--tests",
        "--test-threads=1",
        "--format=json",
        "--env=MIRI_SYSROOT=/state/miri",
    ] {
        assert!(
            tests.arguments.iter().any(|argument| argument == required),
            "{required}"
        );
    }
    let mutation = plan.step(Phase::Mutations);
    assert!(!mutation
        .arguments
        .iter()
        .any(|argument| argument == "--in-place"));
    for required in [
        "mutants",
        "--jobs=1",
        "--jobserver-tasks=1",
        "--baseline=run",
        "--timeout=30",
        "--build-timeout=60",
        "--no-shuffle",
        "src/stasis.rs",
    ] {
        assert!(
            mutation
                .arguments
                .iter()
                .any(|argument| argument == required),
            "{required}"
        );
    }
    assert!(CoreAssurancePlan::new("fixture", "/core", "/state", "latest").is_err());
    assert!(CoreAssurancePlan::new("../escape", "/core", "/state", IMAGE).is_err());
    Ok(())
}

#[test]
fn the_assurance_manifest_binds_all_actual_core_test_targets() -> Result<(), AssuranceError> {
    let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
    let mut targets = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(core.join("tests"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            targets.insert(
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or(AssuranceError::Manifest)?
                    .to_owned(),
            );
        }
    }
    let source = std::fs::read_to_string(core.join("assurance/Cargo.toml"))?;
    let normalized = normalize_manifest(&source, &targets)?;
    let value: toml::Table = toml::from_str(&normalized).map_err(|_| AssuranceError::Manifest)?;
    assert_eq!(
        value
            .get("lib")
            .and_then(|value| value.get("path"))
            .and_then(toml::Value::as_str),
        Some("src/lib.rs")
    );
    for rejected in [
        source.replace("../src/lib.rs", "../src/model.rs"),
        source.replace("../tests/media_tasks.rs", "../tests/fake.rs"),
        source.replace("name = \"media_tasks\"", "name = \"not_selected\""),
        format!("{source}\n[dependencies]\nmodel = \"1\"\n"),
    ] {
        assert!(normalize_manifest(&rejected, &targets).is_err());
    }
    Ok(())
}

const DISCOVERY: &str = concat!(
    "{\"type\":\"suite\",\"event\":\"discovery\"}\n",
    "{\"type\":\"test\",\"event\":\"discovered\",\"name\":\"terminal\",\"ignore\":false}\n",
    "{\"type\":\"test\",\"event\":\"discovered\",\"name\":\"ownership\",\"ignore\":false}\n",
    "{\"type\":\"suite\",\"event\":\"completed\",\"tests\":2,\"benchmarks\":0,\"total\":2,\"ignored\":0}\n",
);
const EXECUTION: &str = concat!(
    "{\"type\":\"suite\",\"event\":\"started\",\"test_count\":2}\n",
    "{\"type\":\"test\",\"event\":\"started\",\"name\":\"terminal\"}\n",
    "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"terminal\"}\n",
    "{\"type\":\"test\",\"event\":\"started\",\"name\":\"ownership\"}\n",
    "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"ownership\"}\n",
    "{\"type\":\"suite\",\"event\":\"ok\",\"passed\":2,\"failed\":0,\"ignored\":0,\"measured\":0,\"filtered_out\":0}\n",
);

#[test]
fn miri_requires_every_discovered_test_to_run_without_skips() -> Result<(), AssuranceError> {
    let expected = ["terminal".to_owned(), "ownership".to_owned()]
        .into_iter()
        .collect();
    let inventory = validate_miri_discovery(DISCOVERY.as_bytes(), &expected, 1)?;
    validate_miri_execution(EXECUTION.as_bytes(), &inventory)?;
    for rejected in [
        DISCOVERY.replace("false", "true"),
        DISCOVERY.replace("ownership", "terminal"),
        DISCOVERY.replace("\"tests\":2", "\"tests\":0"),
        String::new(),
    ] {
        assert!(validate_miri_discovery(rejected.as_bytes(), &expected, 1).is_err());
    }
    for rejected in [
        EXECUTION.replace("\"filtered_out\":0", "\"filtered_out\":1"),
        EXECUTION.replace(
            "\"event\":\"ok\",\"name\":\"ownership\"",
            "\"event\":\"ignored\",\"name\":\"ownership\"",
        ),
        EXECUTION.replace("ownership", "terminal"),
        EXECUTION.replace("\"failed\":0", "\"failed\":1"),
        String::new(),
    ] {
        assert!(validate_miri_execution(rejected.as_bytes(), &inventory).is_err());
    }
    Ok(())
}

fn mutants() -> Vec<serde_json::Value> {
    [("src/lib.rs", "Call::apply"), ("src/lib.rs", "Call::claim_connection"), ("src/stasis.rs", "Call::enter_stasis"), ("src/lib.rs", "Call::join"), ("src/media_tasks.rs", "MediaTasks::cancel"), ("src/resource_subscriptions.rs", "ResourceSubscriptions<N>::subscribe")]
        .into_iter().enumerate().map(|(index, (file, function))| serde_json::json!({"name":format!("{file}:{}:1: representative {function}", index + 1), "file":file, "function":{"function_name":function}, "replacement":"false"})).collect()
}

fn phases(success: bool) -> Vec<serde_json::Value> {
    [serde_json::json!({"phase":"Build", "process_status":"Success"}), serde_json::json!({"phase":"Test", "process_status":if success { serde_json::json!("Success") } else { serde_json::json!({"Failure":101}) }})].into_iter().collect()
}

fn caught_report(mutants: &[serde_json::Value]) -> serde_json::Value {
    let mut outcomes: Vec<serde_json::Value> = [serde_json::json!({"scenario":"Baseline", "summary":"Success", "phase_results":phases(true)})].into_iter().collect();
    outcomes.extend(mutants.iter().map(|mutant| serde_json::json!({"scenario":{"Mutant":mutant}, "summary":"CaughtMutant", "phase_results":phases(false)})));
    serde_json::json!({"cargo_mutants_version":"27.1.0", "end_time":"2026-11-16T00:00:00Z", "total_mutants":mutants.len(), "missed":0, "caught":mutants.len(), "timeout":0, "unviable":0, "success":0, "outcomes":outcomes})
}

#[test]
fn mutation_results_require_a_complete_inventory_and_a_successful_baseline(
) -> Result<(), AssuranceError> {
    let mutants = mutants();
    let inventory_json = serde_json::to_vec(&mutants).map_err(AssuranceError::Json)?;
    let inventory = validate_mutation_inventory(&inventory_json)?;
    let complete = caught_report(&mutants);
    validate_mutation_outcomes(
        &serde_json::to_vec(&complete).map_err(AssuranceError::Json)?,
        &inventory,
    )?;
    assert!(validate_mutation_inventory(b"[]").is_err());
    assert!(validate_mutation_inventory(
        &serde_json::to_vec(&mutants[1..]).map_err(AssuranceError::Json)?
    )
    .is_err());
    for (pointer, value) in [
        ("/cargo_mutants_version", serde_json::json!("27.0.0")),
        ("/end_time", serde_json::Value::Null),
        ("/total_mutants", serde_json::json!(0)),
        ("/missed", serde_json::json!(1)),
        ("/timeout", serde_json::json!(1)),
        ("/outcomes/0/summary", serde_json::json!("Failure")),
        ("/outcomes/1/summary", serde_json::json!("MissedMutant")),
        (
            "/outcomes/1/phase_results/1/process_status",
            serde_json::json!("Success"),
        ),
        (
            "/outcomes/1/scenario/Mutant/name",
            serde_json::json!("unexpected"),
        ),
    ] {
        let mut rejected = complete.clone();
        *rejected
            .pointer_mut(pointer)
            .ok_or(AssuranceError::Results)? = value;
        assert!(
            validate_mutation_outcomes(
                &serde_json::to_vec(&rejected).map_err(AssuranceError::Json)?,
                &inventory
            )
            .is_err(),
            "{pointer}"
        );
    }
    Ok(())
}

#[test]
fn mutation_inventory_names_the_missing_or_unrecognized_invariant() -> Result<(), AssuranceError> {
    let mut missing = mutants();
    missing.retain(|mutant| mutant["file"] != "src/stasis.rs");
    let error =
        validate_mutation_inventory(&serde_json::to_vec(&missing).map_err(AssuranceError::Json)?)
            .err()
            .ok_or(AssuranceError::Inventory)?;
    assert!(error.to_string().contains("Stasis application"));
    let mut unknown = mutants();
    unknown.push(serde_json::json!({"name":"src/stasis.rs:999:1: unknown", "file":"src/stasis.rs", "function":{"function_name":"Other::enter_stasis"}, "replacement":"false"}));
    let error =
        validate_mutation_inventory(&serde_json::to_vec(&unknown).map_err(AssuranceError::Json)?)
            .err()
            .ok_or(AssuranceError::Inventory)?;
    assert!(error.to_string().contains("src/stasis.rs"));
    assert!(error.to_string().contains("Other::enter_stasis"));
    let error = validate_mutation_inventory(b"[]")
        .err()
        .ok_or(AssuranceError::Inventory)?;
    assert!(error.to_string().contains("empty"));
    Ok(())
}

#[test]
fn mutation_results_name_invariants_without_a_caught_viable_mutation() -> Result<(), AssuranceError>
{
    let mutants = mutants();
    let inventory =
        validate_mutation_inventory(&serde_json::to_vec(&mutants).map_err(AssuranceError::Json)?)?;
    let mut report = caught_report(&mutants);
    report["caught"] = serde_json::json!(mutants.len() - 1);
    report["unviable"] = serde_json::json!(1);
    report["outcomes"][1]["summary"] = serde_json::json!("Unviable");
    report["outcomes"][1]["phase_results"] = serde_json::Value::Array(
        [serde_json::json!({"phase":"Build", "process_status":{"Failure":101}})].into(),
    );
    let error = validate_mutation_outcomes(
        &serde_json::to_vec(&report).map_err(AssuranceError::Json)?,
        &inventory,
    )
    .err()
    .ok_or(AssuranceError::Results)?;
    assert!(error.to_string().contains("terminal cleanup"));
    Ok(())
}
