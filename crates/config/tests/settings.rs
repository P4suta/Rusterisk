use rusterisk_config::{Config, ConfigError, Dialplan, Ini, Reload};

const HTTP: &str = "[general]\nenabled=yes\nbindaddr=127.0.0.1\nbindport=8088\n";
const ARI: &str = "[general]\nenabled=yes\nallowed_origins=https://localhost\n[test]\ntype=user\npassword=change-at-install\nread_only=no\n";
const SIP: &str = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[phone-auth]\ntype=auth\nauth_type=userpass\nusername=phone\npassword=local-test-password\n[phone-aor]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=phone-auth\naors=phone-aor\ncontext=internal\ndisallow=all\nallow=ulaw,alaw,opus\n";

#[test]
fn bundled_configuration_keeps_management_local_and_defines_both_example_phones(
) -> Result<(), ConfigError> {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let config = Config::load(&directory)?;
    assert!(config.http.bind.ip().is_loopback());
    assert!(config
        .users
        .iter()
        .any(|user| user.name == "api" && !user.read_only));
    for name in ["alice", "bob"] {
        let endpoint = config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == name);
        assert!(endpoint.is_some());
        assert!(config.auth.iter().any(|auth| auth.username == name));
    }
    assert!(config
        .dialplan
        .steps
        .iter()
        .any(|step| step.context == "internal" && step.extension == "100"));
    Ok(())
}

#[test]
fn parses_asterisk_sections_without_leaking_secrets_in_debug() -> Result<(), ConfigError> {
    let config = Config::parse(
        HTTP,
        ARI,
        SIP,
        "[general]\nrtpstart=10000\nrtpend=10100\n",
        "[internal]\nexten => 100,1,Stasis(test)\n",
    )?;
    assert!(config.http.bind.ip().is_loopback());
    assert_eq!(config.endpoints.len(), 1);
    assert!(!format!("{config:?}").contains("local-test-password"));
    assert!(!format!("{config:?}").contains("change-at-install"));
    Ok(())
}

#[test]
fn refuses_unknown_options_with_their_position() {
    let result = Config::parse(&format!("{HTTP}mystery=yes\n"), ARI, SIP, "", "");
    assert!(matches!(result, Err(ConfigError { line: 5, .. })));
}

#[test]
fn external_management_requires_tls() {
    let http = "[general]\nenabled=yes\nbindaddr=0.0.0.0\nbindport=8088\n";
    assert!(Config::parse(http, ARI, SIP, "", "").is_err());
}

#[test]
fn accepts_canonical_secure_and_loopback_origins() -> Result<(), ConfigError> {
    for origin in [
        "https://phone.example",
        "https://phone.example:8443",
        "http://localhost:8088",
        "http://127.0.0.2:8088",
        "http://[::1]:8088",
    ] {
        let ari = ARI.replace("https://localhost", origin);
        let config = Config::parse(HTTP, &ari, SIP, "", "")?;
        assert_eq!(config.origins, [origin], "{origin}");
    }
    Ok(())
}

#[test]
fn rejects_noncanonical_origins_and_misleading_loopback_names() {
    for origin in [
        "http://localhost.evil.example",
        "http://127.0.0.1.evil.example",
        "http://localhost@evil.example",
        "https://user@phone.example",
        "https://phone.example/path",
        "https://phone.example/",
        "https://phone.example?query",
        "https://phone.example#fragment",
        "https://phone.example:443",
        "https://PHONE.example",
        "https://*.example",
        "http://phone.example",
        "https://",
        "null",
    ] {
        let ari = ARI.replace("https://localhost", origin);
        let result = Config::parse(HTTP, &ari, SIP, "", "");
        assert!(
            matches!(result, Err(ConfigError { line: 3, .. })),
            "{origin}: {result:?}"
        );
    }
}

#[test]
fn reload_is_transactional_and_listener_changes_require_restart() -> Result<(), ConfigError> {
    let first = Config::parse(HTTP, ARI, SIP, "", "")?;
    let changed_auth = Config::parse(
        HTTP,
        &ARI.replace("change-at-install", "another-password"),
        SIP,
        "",
        "",
    )?;
    assert_eq!(first.reload(&changed_auth), Reload::Apply);
    let moved = Config::parse(&HTTP.replace("8088", "8089"), ARI, SIP, "", "")?;
    assert_eq!(first.reload(&moved), Reload::RestartRequired);
    Ok(())
}

#[test]
fn dialplan_accepts_parallel_pjsip_and_rejects_unsupported_applications() -> Result<(), ConfigError>
{
    let dialplan = Dialplan::parse("[internal]\nexten => 100,1,NoOp(start)\nsame => n,Set(X=${EXTEN})\nsame => n,Dial(PJSIP/alice&PJSIP/bob,15)\nsame => n,Hangup()\n")?;
    assert_eq!(dialplan.steps.len(), 4);
    assert!(Dialplan::parse("[internal]\nexten => 100,1,Queue(sales)\n").is_err());
    assert!(Dialplan::parse("[internal]\nexten => 100,1,Dial(SIP/alice)\n").is_err());
    Ok(())
}

#[test]
fn dial_without_a_timeout_keeps_the_reference_indefinite_wait() -> Result<(), ConfigError> {
    for (arguments, expected) in [
        ("PJSIP/alice", None),
        ("PJSIP/alice,", None),
        ("PJSIP/alice,15", Some(15)),
    ] {
        let dialplan = Dialplan::parse(&format!("[internal]\nexten => 100,1,Dial({arguments})\n"))?;
        assert!(
            matches!(&dialplan.steps[0].application, rusterisk_config::Application::Dial { timeout, .. } if *timeout == expected)
        );
    }
    for arguments in ["PJSIP/alice,0", "PJSIP/alice,15,g", "PJSIP/alice,invalid"] {
        let error = Dialplan::parse(&format!("[internal]\nexten => 100,1,Dial({arguments})\n"));
        assert!(error.is_err());
        if let Err(error) = error {
            assert_eq!(error.line, 2);
        }
    }
    Ok(())
}

#[test]
fn dialplan_assignments_accept_the_supported_caller_identification_fields(
) -> Result<(), ConfigError> {
    for target in ["CALLERID(num)", "CALLERID(name)", "CALLERID(all)"] {
        let source = format!("[internal]\nexten => 100,1,Set({target}=${{CALLERID(num)}})\n");
        let plan = Dialplan::parse(&source)?;
        assert!(
            matches!(&plan.steps[0].application, rusterisk_config::Application::Set { variable, value } if variable == target && value == "${CALLERID(num)}")
        );
    }
    for target in ["CALLERID(ani)", "CALLERID()", "GLOBAL(X)", "SHELL(command)"] {
        let result = Dialplan::parse(&format!("[internal]\nexten => 100,1,Set({target}=value)\n"));
        assert!(result.is_err(), "unsupported assignment must fail");
        if let Err(error) = result {
            assert_eq!(error.file, "extensions.conf");
            assert_eq!(error.line, 2);
        }
    }
    Ok(())
}

#[test]
fn rejects_includes_duplicate_sections_and_oversized_inputs() {
    assert!(Ini::parse("pjsip.conf", "#include private.conf\n").is_err());
    assert!(Ini::parse("pjsip.conf", "[x]\na=1\n[x]\nb=2\n").is_err());
    assert!(Ini::parse("pjsip.conf", &"x".repeat(1_048_577)).is_err());
}

#[test]
fn outbound_credentials_are_explicit_references_separate_from_incoming_authentication(
) -> Result<(), ConfigError> {
    let sip = format!("{}[remote]\ntype=auth\nusername=remote-user\npassword=remote-password\nrealm=peer.example\n", SIP.replace("auth=phone-auth", "auth=phone-auth\noutbound_auth=remote"));
    let config = Config::parse(HTTP, ARI, &sip, "", "")?;
    assert_eq!(config.endpoints[0].outbound_auth, ["remote"]);
    assert_eq!(config.endpoints[0].auth, "phone-auth");
    assert!(Config::parse(
        HTTP,
        ARI,
        &sip.replace("outbound_auth=remote", "outbound_auth=missing"),
        "",
        ""
    )
    .is_err());
    assert!(Config::parse(HTTP, ARI, SIP, "", "")?.endpoints[0]
        .outbound_auth
        .is_empty());
    Ok(())
}
