use super::{caught_report, mutants, IMAGE};
use xtask::core_assurance::{
    validate_mutation_inventory, validate_mutation_outcomes, AssuranceError, CoreAssurancePlan,
    Phase,
};

const METHODS: [&str; 8] = [
    "SettingsLifetimes::slot",
    "SettingsLifetimes::available",
    "SettingsLifetimes::prepare",
    "SettingsLifetimes::cancel",
    "SettingsLifetimes::commit",
    "SettingsLifetimes::retain",
    "SettingsLifetimes::release",
    "SettingsError::into_reference",
];

#[test]
fn both_mutation_phases_select_settings_lifetimes_and_reference_recovery(
) -> Result<(), AssuranceError> {
    let plan = CoreAssurancePlan::new("fixture", "/core", "/state", IMAGE)?;
    for phase in [Phase::MutationInventory, Phase::Mutations] {
        let command = plan.step(phase);
        assert!(command
            .arguments
            .windows(2)
            .any(|pair| pair == ["--file", "src/settings_lifetimes.rs"]));
        let selector = command
            .arguments
            .windows(2)
            .find_map(|pair| (pair[0] == "--re").then_some(pair[1].as_str()))
            .ok_or(AssuranceError::Plan)?;
        assert!(selector
            .contains("SettingsLifetimes::(slot|available|prepare|cancel|commit|retain|release)"));
        assert!(selector.contains("SettingsError::into_reference"));
    }
    Ok(())
}

#[test]
fn prior_mutation_scopes_cannot_replace_settings_ownership() -> Result<(), AssuranceError> {
    let mut prior = mutants();
    prior.retain(|mutant| mutant["file"] != "src/settings_lifetimes.rs");
    let error =
        validate_mutation_inventory(&serde_json::to_vec(&prior).map_err(AssuranceError::Json)?)
            .err()
            .ok_or(AssuranceError::Inventory)?;
    assert!(error.to_string().contains("settings ownership"));
    Ok(())
}

#[test]
fn native_inventory_accepts_only_declared_settings_ownership_methods() -> Result<(), AssuranceError>
{
    for method in METHODS {
        let mut inventory = mutants();
        inventory.push(serde_json::json!({
            "name":format!("src/settings_lifetimes.rs:1:1: replace {method}"),
            "file":"src/settings_lifetimes.rs",
            "function":{"function_name":method},
            "replacement":"false",
        }));
        validate_mutation_inventory(
            &serde_json::to_vec(&inventory).map_err(AssuranceError::Json)?,
        )?;
        let selected = inventory.last_mut().ok_or(AssuranceError::Inventory)?;
        selected["function"]["function_name"] = "SettingsLifetimes::unrecognized".into();
        assert!(validate_mutation_inventory(
            &serde_json::to_vec(&inventory).map_err(AssuranceError::Json)?
        )
        .is_err());
    }
    Ok(())
}

#[test]
fn every_settings_selector_names_an_actual_nonconstant_method() -> Result<(), AssuranceError> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/core/src/settings_lifetimes.rs");
    let file = syn::parse_file(&std::fs::read_to_string(path)?).map_err(AssuranceError::Syntax)?;
    let mut methods = std::collections::BTreeSet::new();
    for item in file.items {
        let syn::Item::Impl(implementation) = item else {
            continue;
        };
        let syn::Type::Path(path) = &*implementation.self_ty else {
            continue;
        };
        if !path.path.is_ident("SettingsLifetimes") && !path.path.is_ident("SettingsError") {
            continue;
        }
        let name = path.path.get_ident().ok_or(AssuranceError::Inventory)?;
        for item in implementation.items {
            let syn::ImplItem::Fn(function) = item else {
                continue;
            };
            if function.sig.constness.is_none() {
                methods.insert(format!("{name}::{}", function.sig.ident));
            }
        }
    }
    assert_eq!(methods, METHODS.into_iter().map(str::to_owned).collect());
    Ok(())
}

#[test]
fn other_caught_scopes_require_a_viable_settings_mutation() -> Result<(), AssuranceError> {
    let mutants = mutants();
    let inventory =
        validate_mutation_inventory(&serde_json::to_vec(&mutants).map_err(AssuranceError::Json)?)?;
    let mut report = caught_report(&mutants);
    let mut unviable = 0;
    for (index, mutant) in mutants.iter().enumerate() {
        if mutant["file"] == "src/settings_lifetimes.rs" {
            report["outcomes"][index + 1]["summary"] = serde_json::json!("Unviable");
            report["outcomes"][index + 1]["phase_results"] = serde_json::Value::Array(
                [serde_json::json!({"phase":"Build", "process_status":{"Failure":101}})].into(),
            );
            unviable += 1;
        }
    }
    assert_eq!(unviable, METHODS.len());
    report["caught"] = serde_json::json!(mutants.len() - unviable);
    report["unviable"] = serde_json::json!(unviable);
    let error = validate_mutation_outcomes(
        &serde_json::to_vec(&report).map_err(AssuranceError::Json)?,
        &inventory,
    )
    .err()
    .ok_or(AssuranceError::Results)?;
    assert!(error.to_string().contains("settings ownership"));
    Ok(())
}
