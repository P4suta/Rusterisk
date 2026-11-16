use rusterisk_config::{
    Config, ConfigError, Protocol, RegistrarUri, RegistrationAddress, RegistrationUriError,
};

const AUTH: &str = "[credential]\ntype=auth\nusername=account\npassword=sample\n";

fn source(protocol: &str, server: &str, client: &str) -> String {
    let server = server.replace(';', "\\;");
    let client = client.replace(';', "\\;");
    let tls = if protocol == "tls" {
        "cert_file=certificate.pem\npriv_key_file=private.pem\n"
    } else {
        ""
    };
    format!("[provider]\ntype=registration\ntransport=stream\noutbound_auth=credential\nserver_uri={server}\nclient_uri={client}\n[stream]\ntype=transport\nprotocol={protocol}\nbind=127.0.0.1:5060\n{tls}{AUTH}")
}

fn parse(source: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        source,
        "",
        "",
    )
}

fn refused(server: &str, client: &str, line: usize) {
    let result = parse(&source("udp", server, client));
    assert!(
        matches!(result, Err(error) if error.file == "pjsip.conf" && error.line == line),
        "server={server:?}, client={client:?}"
    );
}

#[test]
fn registrar_and_address_roles_reject_user_information_and_passwords() {
    for server in [
        "sip:account@provider.example",
        "sip:account:sample@provider.example",
        "sip:@provider.example",
    ] {
        refused(server, "sip:account@provider.example", 5);
    }
    for client in [
        "sip:account:sample@provider.example",
        "sip:account:@provider.example",
        "sip:@provider.example",
        "sip:account@other@provider.example",
    ] {
        refused("sip:provider.example", client, 6);
    }
}

#[test]
fn malformed_uri_boundaries_and_percent_escapes_keep_the_option_position() {
    for server in [
        "",
        "provider.example",
        "http://provider.example",
        "sip:",
        "<sip:provider.example>",
        "sip:provider.example/path",
        "sip:provider.example#fragment",
        "sip:provider.example:invalid",
        "sip:provider.example:0",
        "sip:provider.example:65536",
        "sip:[::1]ignored",
        "sip:2001:db8::1",
        "sip:provider..example",
        "sip:-provider.example",
        "sip:provider_example",
        "sip:provider%2eexample",
        "sip:provider.example;",
        "sip:provider.example;;extension=value",
        "sip:provider.example;extension=",
        "sip:provider.example;extension=%zz",
        "sip:provider.example;extension=one;Extension=two",
    ] {
        refused(server, "sip:account@provider.example", 5);
    }
    for client in [
        "sip:account%zz@provider.example",
        "sip:account%@provider.example",
        "sip:account%1@provider.example",
        "sip:two accounts@provider.example",
        "sip:account\\value@provider.example",
        "sip:account@provider.example?subject=value",
    ] {
        refused("sip:provider.example", client, 6);
    }
}

#[test]
fn unsupported_uri_effects_fail_before_network_setup() {
    for server in [
        "sip:provider.example?subject=value",
        "sip:provider.example;maddr=192.0.2.1",
        "sip:provider.example;ttl=2",
        "sip:provider.example;method=REGISTER",
        "sip:provider.example;lr",
        "sip:provider.example;transport=tcp",
        "sips:provider.example",
    ] {
        refused(server, "sip:account@provider.example", 5);
    }
    for client in [
        "sip:account@provider.example:5060",
        "sip:account@provider.example;transport=udp",
        "sip:account@provider.example;maddr=192.0.2.1",
        "sip:account@provider.example;method=REGISTER",
        "sip:account@provider.example;lr",
        "sips:account@provider.example",
    ] {
        refused("sip:provider.example", client, 6);
    }
}

#[test]
fn unresolved_references_identify_the_named_option_after_complete_parsing() {
    let original = source(
        "udp",
        "sip:provider.example",
        "sip:account@provider.example",
    );
    for (key, line) in [("transport", 3), ("outbound_auth", 4)] {
        let previous = if key == "transport" {
            "stream"
        } else {
            "credential"
        };
        let changed = original.replacen(&format!("{key}={previous}"), &format!("{key}=missing"), 1);
        assert!(
            matches!(parse(&changed), Err(error) if error.file == "pjsip.conf" && error.line == line),
            "{key}"
        );
    }
}

#[test]
fn oversized_uris_are_refused_at_the_option() {
    let server = format!("sip:{}", "a".repeat(2048));
    refused(&server, "sip:account@provider.example", 5);
    let client = format!("sip:{}@provider.example", "a".repeat(2048));
    refused("sip:provider.example", &client, 6);
}

#[test]
fn constructors_preserve_original_octets_and_distinguish_registrar_transport(
) -> Result<(), RegistrationUriError> {
    for (protocol, source) in [
        (Protocol::Udp, "SIP:Provider.Example:05060;transport=UDP"),
        (Protocol::Udp, "sip:192.0.2.1:5060"),
        (Protocol::Tcp, "sip:[2001:db8::1]:5070;transport=tcp"),
        (Protocol::Tls, "sips:provider.example;transport=tcp"),
        (Protocol::Tls, "sip:provider.example;transport=tls"),
        (Protocol::Udp, "sip:provider.example;extension=one%20two"),
    ] {
        let uri = RegistrarUri::parse(protocol, source)?;
        assert_eq!(uri.as_str(), source);
        assert_eq!(uri.protocol(), protocol);
    }
    for source in [
        "sip:%61ccount@Provider.Example",
        "sip:account@192.0.2.1",
        "sip:account@[2001:db8::1]",
        "sip:account;phone-context=local@provider.example;user=phone",
        "sips:account@provider.example;extension=one%20two",
        "sip:provider.example",
    ] {
        let uri = RegistrationAddress::parse(source)?;
        assert_eq!(uri.as_str(), source);
        assert_eq!(uri.is_secure(), source.starts_with("sips:"));
    }
    Ok(())
}

#[test]
fn checked_constructors_reject_untrimmed_inputs_and_unimplemented_effects() {
    for source in [
        " sip:provider.example",
        "sip:provider.example ",
        "sip:provider.example\r\n",
        "sip:provider.example;transport",
        "sip:provider.example;TRANSPORT=udp;transport=udp",
        "sip:provider.example;transport=udp;trans%70ort=udp",
    ] {
        assert!(RegistrarUri::parse(Protocol::Udp, source).is_err());
    }
    for protocol in [Protocol::Ws, Protocol::Wss] {
        assert_eq!(
            RegistrarUri::parse(protocol, "sip:provider.example"),
            Err(RegistrationUriError::Unsupported)
        );
    }
    assert_eq!(
        RegistrarUri::parse(Protocol::Udp, "sips:provider.example"),
        Err(RegistrationUriError::TransportConflict)
    );
}

#[test]
fn uri_capacity_counts_original_octets_before_component_parsing() -> Result<(), RegistrationUriError>
{
    let prefix = "sip:provider.example;extension=";
    let source = format!("{prefix}{}", "a".repeat(2048 - prefix.len()));
    assert_eq!(
        RegistrarUri::parse(Protocol::Udp, &source)?.as_str(),
        source
    );
    assert_eq!(
        RegistrarUri::parse(Protocol::Udp, &format!("{source}a")),
        Err(RegistrationUriError::Capacity)
    );
    let prefix = "sip:account@provider.example;extension=";
    let source = format!("{prefix}{}", "a".repeat(2048 - prefix.len()));
    assert_eq!(RegistrationAddress::parse(&source)?.as_str(), source);
    assert_eq!(
        RegistrationAddress::parse(&format!("{source}a")),
        Err(RegistrationUriError::Capacity)
    );
    Ok(())
}

#[test]
fn registrar_brackets_are_reserved_for_ipv6_references() {
    for source in [
        "sip:[192.0.2.1]",
        "sip:[192.0.2.1]:5060",
        "sip:[192.0.2.1]ignored",
    ] {
        assert!(
            RegistrarUri::parse(Protocol::Udp, source).is_err(),
            "{source}"
        );
    }
}

#[test]
fn address_brackets_are_reserved_for_ipv6_references() {
    for source in ["sip:account@[192.0.2.1]", "sip:account@[192.0.2.1]ignored"] {
        assert!(RegistrationAddress::parse(source).is_err(), "{source}");
    }
}

#[test]
fn escaped_reserved_parameter_names_remain_distinct_from_literal_names(
) -> Result<(), RegistrationUriError> {
    let source = "sip:provider.example;+name=one;%2Bname=two";
    assert_eq!(RegistrarUri::parse(Protocol::Udp, source)?.as_str(), source);
    let source = "sip:account@provider.example;+name=one;%2Bname=two";
    assert_eq!(RegistrationAddress::parse(source)?.as_str(), source);
    assert!(
        RegistrarUri::parse(Protocol::Udp, "sip:provider.example;name=one;n%61me=two").is_err()
    );
    Ok(())
}

#[test]
fn forward_references_and_secure_registration_keep_checked_uri_values() -> Result<(), ConfigError> {
    for (protocol, server, client) in [
        (
            "udp",
            "sip:provider.example",
            "sip:account@provider.example",
        ),
        (
            "tcp",
            "sip:provider.example;transport=tcp",
            "sip:%61ccount@provider.example",
        ),
        (
            "tls",
            "sips:Provider.Example;transport=tcp",
            "sips:account@provider.example;user=phone",
        ),
    ] {
        let config = parse(&source(protocol, server, client))?;
        assert_eq!(config.registrations[0].server_uri.as_str(), server);
        assert_eq!(config.registrations[0].client_uri.as_str(), client);
    }
    Ok(())
}
