use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{Destination, Engine, SipError, StaticTarget, TargetHost};

#[test]
fn static_targets_preserve_original_hosts_explicit_ports_and_transport_policy(
) -> Result<(), SipError> {
    for (protocol, uri, port) in [
        (Protocol::Udp, "sip:phone@example.test", None),
        (
            Protocol::Tcp,
            "sip:phone@example.test:5070;transport=tcp",
            Some(5070),
        ),
        (Protocol::Tls, "sips:phone@example.test;transport=tcp", None),
        (
            Protocol::Tls,
            "sip:phone@example.test:5061;transport=tcp",
            Some(5061),
        ),
    ] {
        let target = StaticTarget::from_uri(protocol, uri)?;
        assert_eq!(target.protocol(), protocol);
        assert_eq!(target.port(), port);
        assert!(matches!(target.host(), TargetHost::Name(name) if name == "example.test"));
    }
    let target = StaticTarget::from_uri(Protocol::Tcp, "sip:phone@[::1]:5070")?;
    assert!(
        matches!(target.host(), TargetHost::Address(address) if *address == "::1".parse::<std::net::IpAddr>()?)
    );
    assert_eq!(target.port(), Some(5070));
    for (protocol, uri) in [
        (Protocol::Udp, "sips:phone@example.test"),
        (Protocol::Tcp, "sips:phone@example.test"),
        (Protocol::Tcp, "sip:phone@example.test;transport=udp"),
        (Protocol::Tls, "sips:phone@example.test;transport=udp"),
        (Protocol::Tls, "sip:phone@example.test;transport=tls"),
        (Protocol::Tcp, "sip:phone:password@example.test"),
        (Protocol::Tcp, "sip:phone@example.test;maddr=other.test"),
        (Protocol::Tcp, "sip:phone@example.test?Subject=ignored"),
        (Protocol::Ws, "sip:phone@example.test;transport=ws"),
        (Protocol::Wss, "sips:phone@example.test;transport=ws"),
    ] {
        assert!(matches!(
            StaticTarget::from_uri(protocol, uri),
            Err(SipError::Unsupported)
        ));
    }
    for uri in [
        "sip:phone@127.0.0.1:0",
        "sip:phone@0.0.0.0",
        "sip:phone@224.0.0.1",
    ] {
        assert!(StaticTarget::from_uri(Protocol::Tcp, uri).is_err());
    }
    assert!(
        StaticTarget::from_uri(Protocol::Tcp, &format!("sip:phone@{}", "x".repeat(2048))).is_err()
    );
    Ok(())
}

#[test]
fn endpoint_destination_selects_the_configured_static_target_without_network_io(
) -> Result<(), SipError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=password\n[static]\ntype=aor\ncontact=sip:phone@example.test:5070\n[phone]\ntype=endpoint\ntransport=stream\nauth=auth\naors=static\nallow=ulaw\n", "", "")?;
    let engine = Engine::new(config, [7; 32]);
    let Destination::Static(target) = engine.destination("phone", 0)? else {
        return Err(SipError::InvalidPacket);
    };
    assert!(matches!(target.host(), TargetHost::Name(name) if name == "example.test"));
    assert_eq!(target.port(), Some(5070));
    assert_eq!(target.protocol(), Protocol::Tcp);
    assert!(engine.destination("missing", 0).is_err());
    Ok(())
}
