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
    assert!(
        !xtask::inspect_with_exceptions(
            &source.replace("Keep bounded frames inline.", "Unregistered reason."),
            std::slice::from_ref(&exception)
        )?
        .is_empty()
    );
    assert!(
        !xtask::inspect_with_exceptions(
            &source.replace("enum AudioSocket", "enum Other"),
            std::slice::from_ref(&exception)
        )?
        .is_empty()
    );
    assert!(
        !xtask::inspect_with_exceptions(
            &source.replace("#[expect", "#![expect"),
            std::slice::from_ref(&exception)
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn accepts_concrete_owned_values_and_ignores_literal_fixture_text() -> Result<(), syn::Error> {
    let source =
        r#"enum Packet { Audio([u8; 160]), End } fn f<T: Send>(_: T) { let _s = "Arc dyn Box"; }"#;
    assert!(inspect(source)?.is_empty());
    Ok(())
}

fn client_config_exception() -> xtask::Exception {
    xtask::Exception {
        path: "crates/server/src/encryption/client.rs".into(),
        item: "client_connector".into(),
        lint: "clippy::disallowed_types".into(),
        adr: "docs/adr/confine-client-tls-sharing-to-immutable-library-config.md".into(),
        rationale: "Construct only immutable external TLS client configuration.".into(),
    }
}

fn client_config_source() -> &'static str {
    r#"fn client_connector(config: ::tokio_rustls::rustls::ClientConfig) -> ::tokio_rustls::TlsConnector {
        #[expect(clippy::disallowed_types, reason = "docs/adr/confine-client-tls-sharing-to-immutable-library-config.md: Construct only immutable external TLS client configuration.")]
        let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>::new(config);
        ::tokio_rustls::TlsConnector::from(config)
    }"#
}

#[test]
fn permits_only_one_registered_immutable_client_config_constructor() -> Result<(), syn::Error> {
    let exception = client_config_exception();
    assert!(xtask::inspect_with_exceptions(client_config_source(), &[exception])?.is_empty());
    Ok(())
}

#[test]
fn rejects_client_config_exception_escape_routes() -> Result<(), syn::Error> {
    let source = client_config_source();
    let initializer =
        "let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>::new(config);";
    let fixtures = [
        source.replace("fn client_connector", "fn other_connector"),
        source.replace("let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>", "let config = ::std::sync::Arc::<DomainState>"),
        source.replace("let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>", "let config = ::std::sync::Arc::<ClientConfig>"),
        source.replace("::std::sync::Arc::<", "std::sync::Arc::<"),
        source.replace("::std::sync::Arc::<", "Shared::<"),
        source.replace("Construct only immutable external TLS client configuration.", "Unregistered reason."),
        source.replace("#[expect(clippy::disallowed_types,", "#[allow(clippy::disallowed_types,"),
        source.replace("#[expect(clippy::disallowed_types,", "#[expect(clippy::disallowed_types, clippy::unwrap_used,"),
        source.replace(initializer, &format!("{initializer}\n{initializer}")),
        format!("mod nested {{ {source} }}"),
        source.replace("::tokio_rustls::TlsConnector::from(config)", "let _state = ::std::sync::Arc::new(DomainState); ::tokio_rustls::TlsConnector::from(config)"),
        source.replace(initializer, "let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>::new(::std::sync::Arc::new(config));"),
        source.replace("::new(config);", "::new(config!());"),
        format!("extern crate self as tokio_rustls; {source}"),
        format!("extern crate foreign as std; {source}"),
    ];
    for fixture in fixtures {
        assert!(
            !xtask::inspect_with_exceptions(&fixture, &[client_config_exception()])?.is_empty(),
            "{fixture}"
        );
    }
    assert!(!xtask::inspect(source)?.is_empty());
    let attribute = source.lines().nth(1).unwrap_or("");
    assert!(!attribute.is_empty());
    let bare = source.replacen(attribute, "", 1);
    assert!(!xtask::inspect_with_exceptions(&bare, &[client_config_exception()])?.is_empty());
    assert!(
        !xtask::inspect_with_exceptions(
            &format!("{attribute}\n{bare}"),
            &[client_config_exception()]
        )?
        .is_empty()
    );
    Ok(())
}
