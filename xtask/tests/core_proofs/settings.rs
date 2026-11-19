use super::{complete, IMAGE};
use xtask::core_proofs::{
    validate_harnesses, validate_results, CoreProofPlan, ProofError, HARNESSES,
};

const SETTINGS: [&str; 4] = [
    "proofs::settings_references_preserve_their_generation_until_release",
    "proofs::settings_candidates_cannot_cross_domains_or_replace_later_candidates",
    "proofs::settings_foreign_references_leave_both_domains_unchanged",
    "proofs::settings_reference_count_boundaries_preserve_publication",
];

fn bound(name: &str) -> u16 {
    if name == "proofs::settings_reference_count_boundaries_preserve_publication" {
        2
    } else {
        3
    }
}

fn harnesses(selected: &str, selected_bound: u16) -> Result<String, ProofError> {
    HARNESSES
        .iter()
        .chain(SETTINGS.iter())
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let unwind = if name == selected {
                selected_bound
            } else if SETTINGS.contains(&name) {
                bound(name)
            } else {
                12
            };
            let name = name.strip_prefix("proofs::").ok_or(ProofError::Harnesses)?;
            Ok(format!(
                "#[kani::proof]\n#[kani::unwind({unwind})]\nfn {name}() {{ assert!(true); kani::cover!(true); }}\n"
            ))
        })
        .collect()
}

#[test]
fn settings_harnesses_require_their_exact_transition_bounds() -> Result<(), ProofError> {
    for name in SETTINGS {
        let expected = bound(name);
        validate_harnesses(&harnesses(name, expected)?)?;
        for rejected in [expected - 1, expected + 1] {
            assert!(
                validate_harnesses(&harnesses(name, rejected)?).is_err(),
                "{name}: {rejected}"
            );
        }
    }
    Ok(())
}

#[test]
fn existing_harnesses_keep_their_original_bound_range() -> Result<(), ProofError> {
    for name in HARNESSES
        .into_iter()
        .filter(|name| !SETTINGS.contains(name))
    {
        for bound in [1, 32] {
            validate_harnesses(&harnesses(name, bound)?)?;
        }
        assert!(validate_harnesses(&harnesses(name, 66)?).is_err(), "{name}");
    }
    Ok(())
}

#[test]
fn native_proofs_select_every_settings_ownership_contract() -> Result<(), ProofError> {
    let plan = CoreProofPlan::new("fixture", "/project/core", "/project/results", IMAGE)?;
    let command = plan.run();
    for harness in SETTINGS {
        assert!(
            command.arguments.iter().any(|argument| argument == harness),
            "{harness}"
        );
    }
    Ok(())
}

#[test]
fn reports_without_settings_ownership_cannot_complete_the_current_proof_scope(
) -> Result<(), ProofError> {
    let mut prior = complete();
    let results = prior["verification_results"]["results"]
        .as_array_mut()
        .ok_or(ProofError::Results)?;
    results.retain(|result| {
        !result["harness_id"]
            .as_str()
            .is_some_and(|name| SETTINGS.contains(&name))
    });
    let count = results.len();
    let summary = &mut prior["verification_results"]["summary"];
    summary["total_harnesses"] = count.into();
    summary["executed"] = count.into();
    summary["successful"] = count.into();
    assert!(
        validate_results(&serde_json::to_vec(&prior).map_err(|_| ProofError::Results)?).is_err()
    );
    Ok(())
}
