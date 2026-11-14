use xtask::inspect;

#[test]
fn rejects_dynamic_dispatch_and_shared_ownership_in_all_spellings() -> Result<(), syn::Error> {
    let fixtures = [
        ("fn f(_: &dyn Send) {}", "dynamic-dispatch"),
        ("fn f(_: &'static dyn Send) {}", "dynamic-dispatch"),
        ("type Shared = std::sync::Arc<u8>;", "shared-ownership"),
        (
            "use std::sync::Arc as Shared; type X = Shared<u8>;",
            "shared-ownership",
        ),
        ("struct Wrap<T>(std::boxed::Box<T>);", "heap-erasure"),
        (
            "type Future = core::pin::Pin<std::boxed::Box<dyn core::future::Future<Output = ()>>>;",
            "heap-erasure",
        ),
        (
            "#[async_trait::async_trait] trait X { async fn call(&self); }",
            "async-trait",
        ),
        (
            "#[allow(clippy::unwrap_used)] fn f() {}",
            "lint-suppression",
        ),
        (
            "#[expect(clippy::unwrap_used)] fn f() {}",
            "lint-suppression",
        ),
        ("fn f() { let _x = vec![socket]; }", "single-element-vector"),
        (
            "fn f() { tokio::sync::mpsc::unbounded_channel::<u8>(); }",
            "unbounded-queue",
        ),
    ];
    for (source, rule) in fixtures {
        let findings = inspect(source)?;
        assert!(
            findings.iter().any(|finding| finding.rule == rule),
            "{rule}: {source}: {findings:?}"
        );
    }
    Ok(())
}

#[test]
fn permits_only_registered_item_exceptions_with_the_exact_reason() -> Result<(), syn::Error> {
    let exception = xtask::Exception {
        path: "crates/media/src/lib.rs".into(),
        item: "AudioSocket".into(),
        lint: "clippy::large_enum_variant".into(),
        adr: "docs/adr/serialize-call-decisions-in-a-pure-core.md".into(),
        rationale: "Keep bounded frames inline.".into(),
    };
    let source = r#"#[expect(clippy::large_enum_variant, reason = "docs/adr/serialize-call-decisions-in-a-pure-core.md: Keep bounded frames inline.")] enum AudioSocket { End, Audio([i16; 960]) }"#;
    assert!(xtask::inspect_with_exceptions(source, std::slice::from_ref(&exception))?.is_empty());
    assert!(!xtask::inspect_with_exceptions(
        &source.replace("Keep bounded frames inline.", "Unregistered reason."),
        std::slice::from_ref(&exception)
    )?
    .is_empty());
    assert!(!xtask::inspect_with_exceptions(
        &source.replace("enum AudioSocket", "enum Other"),
        std::slice::from_ref(&exception)
    )?
    .is_empty());
    assert!(!xtask::inspect_with_exceptions(
        &source.replace("#[expect", "#![expect"),
        std::slice::from_ref(&exception)
    )?
    .is_empty());
    Ok(())
}

#[test]
fn accepts_concrete_owned_values_and_ignores_literal_fixture_text() -> Result<(), syn::Error> {
    let source =
        r#"enum Packet { Audio([u8; 160]), End } fn f<T: Send>(_: T) { let _s = "Arc dyn Box"; }"#;
    assert!(inspect(source)?.is_empty());
    Ok(())
}
