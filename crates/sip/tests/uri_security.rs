use rsipstack::sip::{SipMessage, Uri};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{
    parse_registration_contacts, Engine, Outbound, RegistrationContacts, SipError, StaticTarget,
};
#[path = "uri_security/incoming.rs"]
mod incoming;
#[path = "uri_security/outgoing.rs"]
mod outgoing;
#[path = "uri_security/registration.rs"]
mod registration;

fn configuration(protocol: Protocol, target: &str) -> Result<Config, SipError> {
    let target = target.replace(';', "\\;");
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
    Ok(Config::parse(
        http,
        "[app]\ntype=user\npassword=test\n",
        &format!("[native]\ntype=transport\nprotocol={name}\nbind={bind}\n{files}[auth]\ntype=auth\nusername=phone\npassword=password\n[target]\ntype=aor\ncontact={target}\n[phone]\ntype=endpoint\ntransport=native\nauth=auth\naors=target\nallow=ulaw\n"),
        "",
        "",
    )?)
}

fn originate(protocol: Protocol, target: &str) -> Result<(String, std::net::SocketAddr), SipError> {
    let config = configuration(protocol, target)?;
    originate_config(protocol, config)
}

fn originate_mutated_target(
    protocol: Protocol,
    target: &str,
) -> Result<(String, std::net::SocketAddr), SipError> {
    let mut config = configuration(protocol, "sip:phone@127.0.0.2:5070")?;
    config
        .aors
        .first_mut()
        .ok_or(SipError::InvalidPacket)?
        .contact = Some(target.into());
    originate_config(protocol, config)
}

fn originate_config(
    protocol: Protocol,
    config: Config,
) -> Result<(String, std::net::SocketAddr), SipError> {
    let local = config
        .transports
        .first()
        .ok_or(SipError::InvalidPacket)?
        .bind;
    let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
    let output = engine.originate(
        Outbound {
            id: "address-security".into(),
            generation: 1,
            endpoint: "phone".into(),
            caller: "100".into(),
            local_rtp: "127.0.0.1:10000".parse()?,
        },
        0,
    )?;
    let packet = output
        .datagrams
        .into_iter()
        .next()
        .ok_or(SipError::InvalidPacket)?;
    Ok((
        String::from_utf8(packet.bytes).map_err(|_| SipError::InvalidPacket)?,
        local,
    ))
}

#[test]
fn sip_over_tls_keeps_the_sip_scheme_and_uses_the_tcp_uri_parameter() -> Result<(), SipError> {
    let (wire, local) = originate(Protocol::Tls, "sip:phone@127.0.0.2:5070;transport=tcp")?;
    assert!(wire.contains("Via: SIP/2.0/TLS "));
    assert!(wire.contains(&format!(
        "Contact: <sip:rusterisk@{local};transport=tcp>\r\n"
    )));
    assert!(!wire.contains("transport=tls"));
    Ok(())
}

#[test]
fn sip_over_secure_websockets_keeps_the_sip_scheme() -> Result<(), SipError> {
    let (wire, local) = originate(Protocol::Wss, "sip:phone@127.0.0.2:5070;transport=ws")?;
    assert!(wire.contains("Via: SIP/2.0/WSS "));
    assert!(wire.contains(&format!(
        "Contact: <sip:rusterisk@{local};transport=ws>\r\n"
    )));
    Ok(())
}

#[test]
fn sips_contacts_preserve_address_security_over_tls_and_secure_websockets() -> Result<(), SipError>
{
    for (protocol, parameter) in [(Protocol::Tls, "tcp"), (Protocol::Wss, "ws")] {
        let target = format!("sips:phone@127.0.0.2:5070;transport={parameter}");
        let (wire, local) = originate(protocol, &target)?;
        let SipMessage::Request(request) = SipMessage::try_from(wire.as_bytes())? else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request.uri, Uri::try_from(target.as_str())?);
        assert!(wire.contains(&format!(
            "Contact: <sips:rusterisk@{local};transport={parameter}>\r\n"
        )));
    }
    Ok(())
}

#[test]
fn sips_targets_never_generate_requests_over_unencrypted_transports() -> Result<(), SipError> {
    for protocol in [Protocol::Udp, Protocol::Tcp, Protocol::Ws] {
        assert!(matches!(
            originate_mutated_target(protocol, "sips:phone@127.0.0.2:5070"),
            Err(SipError::Unsupported)
        ));
    }
    Ok(())
}

#[test]
fn legacy_transport_parameters_remain_parseable_without_becoming_generated_targets(
) -> Result<(), SipError> {
    let legacy = "sip:phone@127.0.0.2:5070;transport=tls";
    let parsed = parse_registration_contacts([format!("<{legacy}>").as_str()], Some(3600))
        .map_err(|_| SipError::InvalidPacket)?;
    let RegistrationContacts::Bindings(bindings) = parsed else {
        return Err(SipError::InvalidPacket);
    };
    let [contact] = bindings.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(contact.source_uri(), legacy);
    assert!(matches!(
        originate_mutated_target(Protocol::Tls, legacy),
        Err(SipError::Unsupported)
    ));
    Ok(())
}

#[test]
fn native_tls_destination_metadata_accepts_the_tcp_parameter_for_sip_uris() -> Result<(), SipError>
{
    let target = StaticTarget::from_uri(Protocol::Tls, "sip:phone@127.0.0.2:5070;transport=tcp")?;
    assert_eq!(target.protocol(), Protocol::Tls);
    assert!(matches!(
        StaticTarget::from_uri(Protocol::Tls, "sip:phone@127.0.0.2:5070;transport=tls"),
        Err(SipError::Unsupported)
    ));
    Ok(())
}
