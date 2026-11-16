use rusterisk_sip::{ReferError, Replaces, MAX_MESSAGE};

#[test]
fn a_replacement_selector_preserves_the_call_id_and_both_case_sensitive_tags(
) -> Result<(), ReferError> {
    for source in [
        "dialog@host;to-tag=Local;from-tag=Remote",
        "\t dialog@host \t; FROM-TAG = Remote ; TO-TAG = Local\t",
    ] {
        let selector = Replaces::parse(source)?;
        assert_eq!(selector.call_id(), "dialog@host");
        assert_eq!(selector.to_tag(), "Local");
        assert_eq!(selector.from_tag(), "Remote");
        assert!(!selector.early_only());
    }
    Ok(())
}

#[test]
fn early_only_and_generic_parameters_do_not_change_dialog_identity() -> Result<(), ReferError> {
    let selector = Replaces::parse(
        "dialog;from-tag=0;early-only;to-tag=local;x=\"semi;colon\";extension=kept",
    )?;
    assert_eq!(selector.call_id(), "dialog");
    assert_eq!(selector.to_tag(), "local");
    assert_eq!(selector.from_tag(), "0");
    assert!(selector.early_only());
    assert!(Replaces::parse("dialog;to-tag=local;from-tag=remote;EARLY-ONLY")?.early_only());
    Ok(())
}

#[test]
fn call_ids_follow_the_sip_word_grammar_without_normalizing_the_value() -> Result<(), ReferError> {
    for call_id in ["dialog", "Case-Sensitive@host", r#"id/[]{}<>:()\"@host"#] {
        let selector = Replaces::parse(&format!("{call_id};to-tag=local;from-tag=remote"))?;
        assert_eq!(selector.call_id(), call_id);
    }
    for call_id in ["", "@host", "dialog@", "a@@b", "a b", "a,b", "nonascii-ä"] {
        assert_eq!(
            Replaces::parse(&format!("{call_id};to-tag=local;from-tag=remote")),
            Err(ReferError::Invalid),
            "{call_id:?}",
        );
    }
    Ok(())
}

#[test]
fn selectors_require_exactly_one_nonempty_token_for_each_dialog_tag() {
    for source in [
        "dialog",
        "dialog;to-tag=local",
        "dialog;from-tag=remote",
        "dialog;to-tag=;from-tag=remote",
        "dialog;to-tag=local;from-tag=",
        "dialog;to-tag;from-tag=remote",
        "dialog;to-tag=\"local\";from-tag=remote",
        "dialog;to-tag=local;from-tag=remote tag",
        "dialog;to-tag=local;from-tag=remote;TO-TAG=local",
        "dialog;to-tag=local;from-tag=remote;From-Tag=other",
        "dialog;to-tag=local;from-tag=remote;early-only=true",
        "dialog;to-tag=local;from-tag=remote;early-only;Early-Only",
        "dialog;to-tag=local;from-tag=remote;x=one;X=two",
    ] {
        assert_eq!(
            Replaces::parse(source),
            Err(ReferError::Invalid),
            "{source}"
        );
    }
}

#[test]
fn malformed_parameter_framing_and_header_injection_are_rejected() {
    for source in [
        "dialog;to-tag=local;from-tag=remote;",
        "dialog;to-tag=local;;from-tag=remote",
        "dialog;to-tag=local;from-tag=remote;x=\"unclosed",
        "dialog;to-tag=local;from-tag=remote;x=\"quoted\"tail",
        "dialog;to-tag=local;from-tag=remote;x=",
        "dialog;to-tag=local;from-tag=remote;=value",
        "dialog;to-tag=local;from-tag=remote\r\nInjected: header",
        "dialog;to-tag=local;from-tag=remote\n",
        "dialog\0;to-tag=local;from-tag=remote",
        "dialog;to-tag=local;from-tag=remote;x=\"control\u{7f}\"",
        "\u{2000}dialog;to-tag=local;from-tag=remote",
    ] {
        assert_eq!(
            Replaces::parse(source),
            Err(ReferError::Invalid),
            "{source:?}"
        );
    }
}

#[test]
fn parameter_capacity_includes_both_tags() -> Result<(), ReferError> {
    for (extra, accepted) in [(30, true), (31, false)] {
        let extension = (0..extra)
            .map(|index| format!(";x-{index}=value"))
            .collect::<String>();
        let parsed = Replaces::parse(&format!("dialog;to-tag=local;from-tag=remote{extension}"));
        if accepted {
            assert_eq!(parsed?.call_id(), "dialog");
        } else {
            assert_eq!(parsed, Err(ReferError::Capacity));
        }
    }
    Ok(())
}

#[test]
fn selector_bytes_are_bounded_before_field_copies() -> Result<(), ReferError> {
    let suffix = ";to-tag=local;from-tag=remote";
    let source = format!("{}{suffix}", "x".repeat(MAX_MESSAGE - suffix.len()));
    assert_eq!(
        Replaces::parse(&source)?.call_id().len(),
        MAX_MESSAGE - suffix.len()
    );
    assert_eq!(
        Replaces::parse(&format!("x{source}")),
        Err(ReferError::Capacity),
    );
    Ok(())
}

#[test]
fn selector_debug_output_omits_dialog_identifiers() -> Result<(), ReferError> {
    let selector = Replaces::parse("private-dialog;to-tag=private-local;from-tag=private-remote")?;
    let output = format!("{selector:?}");
    for value in ["private-dialog", "private-local", "private-remote"] {
        assert!(!output.contains(value));
    }
    Ok(())
}
