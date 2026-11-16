use rusterisk_config::{Config, ConfigError, Protocol, RegistrarUri};

fn registration(server: &str, client: &str) -> Result<Config, ConfigError> {
    let server = server.replace(';', "\\;");
    let source = format!("[provider]\ntype=registration\ntransport=native\noutbound_auth=credential\nserver_uri={server}\nclient_uri={client}\n[native]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=certificate.pem\npriv_key_file=private.pem\n[credential]\ntype=auth\nusername=account\npassword=sample\n");
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &source,
        "",
        "",
    )
}

#[test]
fn tls_hops_accept_sip_and_sips_roles_without_a_tls_uri_parameter() -> Result<(), ConfigError> {
    for (server, client) in [
        ("sip:provider.example", "sip:account@provider.example"),
        (
            "sip:provider.example;transport=tcp",
            "sip:account@provider.example",
        ),
        ("sips:provider.example", "sips:account@provider.example"),
        (
            "sips:provider.example;transport=tcp",
            "sips:account@provider.example",
        ),
    ] {
        let config = registration(server, client)?;
        let Some(settings) = config.registrations.first() else {
            return Err(ConfigError {
                file: "pjsip.conf".into(),
                line: 1,
                reason: "missing registration".into(),
            });
        };
        assert_eq!(settings.server_uri.as_str(), server);
        assert_eq!(settings.client_uri.as_str(), client);
        assert_eq!(settings.server_uri.protocol(), Protocol::Tls);
    }
    Ok(())
}

#[test]
fn registration_scheme_requirements_identify_the_conflicting_option() {
    for (server, client, line) in [
        ("sips:provider.example", "sip:account@provider.example", 6),
        ("sip:provider.example", "sips:account@provider.example", 5),
    ] {
        assert!(
            matches!(registration(server, client), Err(error) if error.file == "pjsip.conf" && error.line == line && error.reason.contains("scheme"))
        );
    }
}

#[test]
fn deprecated_transport_targets_are_rejected_at_the_server_uri_option() {
    for (server, client) in [
        (
            "sip:provider.example;transport=tls",
            "sip:account@provider.example",
        ),
        (
            "sips:provider.example;transport=TLS",
            "sips:account@provider.example",
        ),
        (
            "sip:provider.example;%74ransport=tls",
            "sip:account@provider.example",
        ),
    ] {
        assert!(
            matches!(registration(server, client), Err(error) if error.file == "pjsip.conf" && error.line == 5 && error.reason.contains("unsupported"))
        );
        assert!(RegistrarUri::parse(Protocol::Tls, server).is_err());
    }
}
