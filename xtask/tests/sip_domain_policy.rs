fn exception() -> xtask::Exception {
    xtask::Exception {
        path: "crates/server/src/encryption/sip_domain.rs".into(),
        item: "sip_client_configuration".into(),
        lint: "clippy::disallowed_types".into(),
        adr: "docs/adr/verify-sip-domains-with-an-immutable-library-callback.md".into(),
        rationale: "Share only immutable SIP certificate trust through the fixed verifier API."
            .into(),
    }
}

fn source() -> &'static str {
    r#"pub(crate) struct SipDomainVerifier { roots: ::tokio_rustls::rustls::RootCertStore }
    fn sip_client_configuration(roots: ::tokio_rustls::rustls::RootCertStore) -> ::tokio_rustls::rustls::ClientConfig {
        let verifier = SipDomainVerifier { roots };
        #[expect(clippy::disallowed_types, reason = "docs/adr/verify-sip-domains-with-an-immutable-library-callback.md: Share only immutable SIP certificate trust through the fixed verifier API.")]
        let verifier = ::std::sync::Arc::<crate::encryption::sip_domain::SipDomainVerifier>::new(verifier);
        ::tokio_rustls::rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(verifier).with_no_client_auth()
    }"#
}

#[derive(Debug)]
enum RegistrationField {
    Path,
    Item,
    Lint,
    Adr,
    Rationale,
}

#[test]
fn permits_only_the_registered_stateless_sip_verifier_constructor() -> Result<(), syn::Error> {
    assert!(xtask::inspect_with_exceptions(source(), &[exception()])?.is_empty());
    Ok(())
}

#[test]
fn rejects_sip_verifier_aliases_wrappers_added_state_and_scope_escape_routes(
) -> Result<(), syn::Error> {
    let source = source();
    let constructor = "let verifier = ::std::sync::Arc::<crate::encryption::sip_domain::SipDomainVerifier>::new(verifier);";
    let attribute = source
        .lines()
        .find(|line| line.contains("#[expect"))
        .unwrap_or("");
    assert!(!attribute.is_empty());
    let fixtures = [
        source.replace(
            constructor,
            "let config = ::std::sync::Arc::<::tokio_rustls::rustls::ClientConfig>::new(config);",
        ),
        source.replace("fn sip_client_configuration", "fn other_configuration"),
        source.replace(
            "roots: ::tokio_rustls::rustls::RootCertStore }",
            "roots: ::tokio_rustls::rustls::RootCertStore, call_state: u64 }",
        ),
        source.replace(
            "roots: ::tokio_rustls::rustls::RootCertStore }",
            "roots: ForeignRoots }",
        ),
        source.replace(
            "pub(crate) struct SipDomainVerifier { roots: ::tokio_rustls::rustls::RootCertStore }",
            "pub(crate) struct SipDomainVerifier(::tokio_rustls::rustls::RootCertStore);",
        ),
        source.replace(
            "pub(crate) struct SipDomainVerifier { roots: ::tokio_rustls::rustls::RootCertStore }",
            "type SipDomainVerifier = ForeignVerifier;",
        ),
        source.replace(
            "struct SipDomainVerifier {",
            "struct SipDomainVerifier<T> {",
        ),
        source.replace(
            "Arc::<crate::encryption::sip_domain::SipDomainVerifier>",
            "Arc::<VerifierAlias>",
        ),
        source.replace(
            "Arc::<crate::encryption::sip_domain::SipDomainVerifier>",
            "Arc::<OwnCallState>",
        ),
        source.replace("::std::sync::Arc::<", "std::sync::Arc::<"),
        source.replace("::std::sync::Arc::<", "Shared::<"),
        source.replace(
            "crate::encryption::sip_domain::SipDomainVerifier",
            "foreign::encryption::sip_domain::SipDomainVerifier",
        ),
        source.replace("::new(verifier);", "::new(wrapper(verifier));"),
        source.replace(
            "::new(verifier);",
            "::new(::std::sync::Arc::new(verifier));",
        ),
        source.replace("::new(verifier);", "::new(verifier!());"),
        source.replace(
            "Share only immutable SIP certificate trust through the fixed verifier API.",
            "Unregistered reason.",
        ),
        source.replace(attribute, ""),
        source.replace("#[expect(", "#[allow("),
        source.replace(
            "#[expect(clippy::disallowed_types,",
            "#[expect(clippy::disallowed_types, clippy::unwrap_used,",
        ),
        source.replace(
            constructor,
            &format!("{constructor}\n{attribute}\n{constructor}"),
        ),
        source.replace(
            constructor,
            &format!("{constructor}\nlet _state = ::std::sync::Arc::new(OwnCallState);"),
        ),
        source.replace(
            &format!("{attribute}\n        {constructor}"),
            &format!("{{\n{attribute}\n{constructor}\n}}"),
        ),
        format!("{attribute}\n{}", source.replace(attribute, "")),
        format!("mod nested {{ {source} }}"),
        format!("extern crate self as tokio_rustls; {source}"),
    ];
    for fixture in fixtures {
        assert!(
            !xtask::inspect_with_exceptions(&fixture, &[exception()])?.is_empty(),
            "{fixture}"
        );
    }
    assert!(!xtask::inspect(source)?.is_empty());
    for field in [
        RegistrationField::Path,
        RegistrationField::Item,
        RegistrationField::Lint,
        RegistrationField::Adr,
        RegistrationField::Rationale,
    ] {
        let mut registration = exception();
        match field {
            RegistrationField::Path => registration.path = "crates/server/src/other.rs".into(),
            RegistrationField::Item => registration.item = "other_configuration".into(),
            RegistrationField::Lint => registration.lint = "clippy::large_enum_variant".into(),
            RegistrationField::Adr => registration.adr = "docs/adr/other.md".into(),
            RegistrationField::Rationale => registration.rationale = "Unregistered reason.".into(),
        }
        assert!(
            !xtask::inspect_with_exceptions(source, &[registration])?.is_empty(),
            "{field:?}"
        );
    }
    Ok(())
}
