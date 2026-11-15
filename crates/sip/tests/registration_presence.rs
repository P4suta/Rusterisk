use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_config::Config;
use rusterisk_core::ContactPresence;
use rusterisk_sip::{Destination, Engine, Outbound, Output, SipError, SipEvent};

fn config() -> Result<Config, SipError> {
    Ok(Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=password\n[phone]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n", "", "")?)
}

fn register(
    engine: &mut Engine,
    serial: u32,
    now: u64,
    expires: u32,
) -> Result<(Output, String), SipError> {
    register_contact(engine, serial, now, expires, "sip:phone@127.0.0.1:5061")
}

fn register_contact(
    engine: &mut Engine,
    serial: u32,
    now: u64,
    expires: u32,
    contact: &str,
) -> Result<(Output, String), SipError> {
    let uri = Uri::try_from("sip:localhost")?;
    let peer = "127.0.0.1:5061".parse()?;
    let packet = |sequence: u32, authorization: &str| {
        format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-register-{serial}-{sequence};rport\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:phone@localhost>\r\nCall-ID: registration-{serial}\r\nCSeq: {sequence} REGISTER\r\nContact: <{contact}>\r\nExpires: {expires}\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
    };
    let challenge = engine.receive(packet(1, "").as_bytes(), peer, now)?;
    assert!(challenge.events.is_empty());
    let response = match SipMessage::try_from(challenge.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(response) => response,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    assert_eq!(response.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let qop = AuthQop::Auth {
        cnonce: "registration-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"registration-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    let wire = packet(2, &authorization);
    let output = engine.receive(wire.as_bytes(), peer, now + 1)?;
    assert!(output.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    Ok((output, wire))
}

fn presence(output: &Output, expected: ContactPresence) {
    assert!(
        matches!(&output.events[..], [SipEvent::EndpointContact { endpoint, generation: 1, presence }] if endpoint == "phone" && *presence == expected)
    );
}

#[test]
fn registrations_publish_presence_once_per_change_and_expire_without_incoming_traffic(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let (registered, wire) = register(&mut engine, 1, 0, 2)?;
    presence(&registered, ContactPresence::Present);
    assert!(engine
        .receive(wire.as_bytes(), "127.0.0.1:5061".parse()?, 2)?
        .events
        .is_empty());
    assert!(engine.tick(2000)?.events.is_empty());
    presence(&engine.tick(2001)?, ContactPresence::Absent);
    assert!(engine.tick(2002)?.events.is_empty());
    let (registered, _) = register(&mut engine, 2, 3000, 20)?;
    presence(&registered, ContactPresence::Present);
    let (removed, _) = register(&mut engine, 3, 4000, 0)?;
    presence(&removed, ContactPresence::Absent);
    assert!(engine.tick(30_000)?.events.is_empty());
    Ok(())
}

#[test]
fn unrelated_packets_cannot_discard_an_expiration_observation() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    register(&mut engine, 1, 0, 1)?;
    let options = "OPTIONS sip:localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-options\r\nFrom: <sip:phone@localhost>;tag=phone\r\nTo: <sip:localhost>\r\nCall-ID: options\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n";
    engine.receive(options.as_bytes(), "127.0.0.1:5061".parse()?, 1001)?;
    presence(&engine.tick(1001)?, ContactPresence::Absent);
    assert!(engine.tick(1002)?.events.is_empty());
    Ok(())
}

#[test]
fn registered_contact_targets_preserve_browser_identity_and_physical_routing(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    for (serial, contact) in [
        (1, "sip:random-user@browser.invalid;transport=ws"),
        (2, "sip:refreshed-user@replacement.invalid;transport=ws"),
    ] {
        register_contact(&mut engine, serial, u64::from(serial), 1, contact)?;
        assert_eq!(
            engine.registered_peer("phone", 3),
            Some("127.0.0.1:5061".parse()?)
        );
        assert!(
            matches!(engine.destination("phone", 3)?, Destination::Registered(peer) if peer == "127.0.0.1:5061".parse()?)
        );
        let output = engine.originate(
            Outbound {
                id: format!("browser-call-{serial}"),
                generation: u64::from(serial),
                endpoint: "phone".into(),
                caller: "100".into(),
                local_rtp: "127.0.0.1:10000".parse()?,
                timeout_ms: None,
            },
            3,
        )?;
        let packet = &output.datagrams[0];
        assert_eq!(packet.peer, "127.0.0.1:5061".parse()?);
        let request = match SipMessage::try_from(packet.bytes.as_slice())? {
            SipMessage::Request(request) => request,
            SipMessage::Response(_) => return Err(SipError::InvalidPacket),
        };
        assert_eq!(request.uri, Uri::try_from(contact)?);
    }
    assert_eq!(engine.registered_peer("phone", 1003), None);
    register_contact(&mut engine, 3, 2000, 1, "sip:last@browser.invalid")?;
    engine.disconnected("127.0.0.1:5061".parse()?);
    assert_eq!(engine.registered_peer("phone", 2002), None);
    Ok(())
}
