use rusterisk_config::{Config, ConfigError, Protocol};

fn configuration(protocol: Protocol, contact: &str) -> Result<Config, ConfigError> {
    let contact = contact.replace(';', "\\;");
    let (name, bind, files) = match protocol {
        Protocol::Udp => ("udp", "127.0.0.1:5060", ""),
        Protocol::Tcp => ("tcp", "127.0.0.1:5060", ""),
        Protocol::Tls => (
            "tls",
            "127.0.0.1:5061",
            "cert_file=certificate.pem\npriv_key_file=private.pem\n",
        ),
        Protocol::Ws => ("ws", "0.0.0.0", ""),
        Protocol::Wss => ("wss", "0.0.0.0", ""),
    };
    let http = if protocol == Protocol::Wss {
        "[general]\nbindaddr=127.0.0.1\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=certificate.pem\ntlsprivatekey=private.pem\n"
    } else {
        "[general]\nbindaddr=127.0.0.1\n"
    };
    Config::parse(
        http,
        "[app]\ntype=user\npassword=sample\n",
        &format!("[target]\ntype=aor\ncontact={contact}\n[native]\ntype=transport\nprotocol={name}\nbind={bind}\n{files}[credential]\ntype=auth\nusername=phone\npassword=sample\n[phone]\ntype=endpoint\ntransport=native\nauth=credential\naors=target\n"),
        "",
        "",
    )
}

fn rejected_at_contact(protocol: Protocol, contact: &str) {
    assert!(
        matches!(configuration(protocol, contact), Err(error) if error.file == "pjsip.conf" && error.line == 3 && error.reason.contains("Contact")),
        "{protocol:?}: {contact}"
    );
}

#[test]
fn configured_contacts_preserve_supported_target_octets() -> Result<(), ConfigError> {
    for (protocol, contact) in [
        (
            Protocol::Udp,
            "SIP:%61lice@provider.example:05060;opaque=%2F",
        ),
        (Protocol::Tcp, "sip:phone@[2001:db8::1]:5070;transport=tcp"),
        (Protocol::Tls, "sip:phone@provider.example;transport=tcp"),
        (Protocol::Tls, "sips:phone@provider.example;transport=tcp"),
        (Protocol::Ws, "sip:phone@browser.invalid;transport=ws"),
        (Protocol::Wss, "sips:phone@browser.invalid;transport=ws"),
    ] {
        let config = configuration(protocol, contact)?;
        let Some(aor) = config.aors.first() else {
            return Err(ConfigError {
                file: "pjsip.conf".into(),
                line: 1,
                reason: "missing address of record".into(),
            });
        };
        assert_eq!(aor.contact.as_deref(), Some(contact));
    }
    Ok(())
}

#[test]
fn deprecated_contact_transports_report_the_contact_option() {
    for contact in [
        "sip:phone@provider.example;transport=tls",
        "sips:phone@provider.example;transport=TLS",
        "sip:phone@provider.example;%74ransport=tls",
    ] {
        rejected_at_contact(Protocol::Tls, contact);
    }
}

#[test]
fn malformed_or_unsupported_contact_components_report_the_contact_option() {
    for contact in [
        "tel:+12025550123",
        "sip:phone:password@provider.example",
        "sip:phone@provider.example?Subject=test",
        "sip:phone@provider.example;method=INVITE",
        "sip:phone@[192.0.2.1]",
        "sip:phone@provider.example;opaque=%gg",
        "sip:phone@provider.example;transport=tcp;TRANSPORT=tcp",
    ] {
        rejected_at_contact(Protocol::Tcp, contact);
    }
}

#[test]
fn contact_roles_are_checked_after_transport_references_are_resolved() {
    for (protocol, contact) in [
        (Protocol::Udp, "sips:phone@provider.example"),
        (Protocol::Tcp, "sips:phone@provider.example;transport=tcp"),
        (Protocol::Ws, "sips:phone@browser.invalid;transport=ws"),
        (Protocol::Udp, "sip:phone@provider.example;transport=tcp"),
        (Protocol::Tls, "sip:phone@provider.example;transport=udp"),
        (Protocol::Wss, "sip:phone@browser.invalid;transport=tcp"),
    ] {
        rejected_at_contact(protocol, contact);
    }
}

#[test]
fn unused_contacts_still_receive_profile_validation() {
    let source = "[unused]\ntype=aor\ncontact=sip:phone@provider.example\\;transport=tls\n";
    let result = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        source,
        "",
        "",
    );
    assert!(
        matches!(result, Err(error) if error.file == "pjsip.conf" && error.line == 3 && error.reason.contains("Contact"))
    );
}

#[test]
fn contact_capacity_is_checked_at_the_original_option() -> Result<(), ConfigError> {
    let mut contact = String::from("sip:phone@provider.example;opaque=");
    contact.push_str(&"a".repeat(2048 - contact.len()));
    let config = configuration(Protocol::Udp, &contact)?;
    assert_eq!(config.aors[0].contact.as_deref(), Some(contact.as_str()));
    contact.push('a');
    assert!(
        matches!(configuration(Protocol::Udp, &contact), Err(error) if error.file == "pjsip.conf" && error.line == 3 && error.reason.contains("2048"))
    );
    Ok(())
}
