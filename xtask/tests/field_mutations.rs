use xtask::core_assurance::{validate_field_execution, AssuranceError, FieldMutation};

#[test]
fn field_mutations_bind_to_actual_methods_and_reject_selector_drift() -> Result<(), AssuranceError>
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("../crates/core/src/lib.rs"))?;
    for mutation in FieldMutation::ALL {
        let mutated = mutation.apply(&source)?;
        assert_ne!(mutated, source);
        syn::parse_file(&mutated).map_err(AssuranceError::Syntax)?;
        assert!(mutation.apply(&mutated).is_err());
        for invalid in [
            String::new(),
            source.replace("impl Call", "impl OtherCall"),
            source
                .replace("fn apply(", "fn apply_other(")
                .replace("fn claim_connection(", "fn claim_other("),
            format!(
                "{source}\nimpl Call {{ fn duplicate(&mut self) {{ {} }} }}",
                mutation.original()
            ),
            source.replace(mutation.original(), ""),
        ] {
            assert!(mutation.apply(&invalid).is_err());
        }
    }
    Ok(())
}

#[test]
fn field_execution_requires_all_tests_and_a_real_test_failure() -> Result<(), AssuranceError> {
    let names = ["known".to_owned(), "regression".to_owned()]
        .into_iter()
        .collect();
    let source = [
        r#"{"type":"suite","event":"started","test_count":2}"#,
        r#"{"type":"test","event":"started","name":"known"}"#,
        r#"{"type":"test","event":"ok","name":"known"}"#,
        r#"{"type":"test","event":"started","name":"regression"}"#,
        r#"{"type":"test","event":"failed","name":"regression"}"#,
        r#"{"type":"suite","event":"failed","passed":1,"failed":1,"ignored":0,"measured":0,"filtered_out":0}"#,
    ].join("\n");
    validate_field_execution(source.as_bytes(), &names, 1)?;
    for invalid in [
        String::new(),
        source.replace("regression", "unexpected"),
        source.replace("\"ignored\":0", "\"ignored\":1"),
        source.replace("\"filtered_out\":0", "\"filtered_out\":1"),
        source.replace("\"test_count\":2", "\"test_count\":1"),
        source
            .replace("\"event\":\"failed\"", "\"event\":\"ok\"")
            .replace("\"passed\":1,\"failed\":1", "\"passed\":2,\"failed\":0"),
    ] {
        assert!(validate_field_execution(invalid.as_bytes(), &names, 1).is_err());
    }
    Ok(())
}
