use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_config::Config;
use rusterisk_sip::{Engine, Output, SipError};

fn register(target: &str, limit: u16, configured: bool) -> Result<(Engine, Output), SipError> {
    let mut config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[application]\ntype=user\npassword=sample\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=device\npassword=sample\n[permitted]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=credential\naors=permitted\n", "", "")?;
    config.aors[0].max_contacts = limit;
    if !configured {
        config.endpoints[0].aors.clear();
    }
    let mut engine = Engine::new(config, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let uri = Uri::try_from("sip:localhost")?;
    let address = if target.is_empty() {
        "sip:localhost".into()
    } else {
        format!("sip:{target}@localhost")
    };
    let packet = |sequence: u32, authorization: &str| {
        format!("REGISTER {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-binding-{sequence};rport\r\nFrom: Displayed <sip:displayed@localhost>;tag=device\r\nTo: <{address}>\r\nCall-ID: binding-authorization\r\nCSeq: {sequence} REGISTER\r\nContact: <sip:random-contact@{peer}>\r\nExpires: 120\r\nMax-Forwards: 70\r\n{authorization}Content-Length: 0\r\n\r\n")
    };
    let output = engine.receive(packet(1, "").as_bytes(), peer, 0)?;
    let response = match SipMessage::try_from(output.datagrams[0].bytes.as_slice())? {
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
    let quality = AuthQop::Auth {
        cnonce: "binding-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "device",
        password: "sample",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Register,
        qop: Some(&quality),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"device\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, nc=00000001, cnonce=\"binding-client\"\r\n", challenge.realm, challenge.nonce);
    let output = engine.receive(packet(2, &authorization).as_bytes(), peer, 1)?;
    Ok((engine, output))
}

fn status(output: &Output) -> Result<u16, SipError> {
    match SipMessage::try_from(output.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response.status_code.code()),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

#[test]
fn registration_targets_use_configured_addresses_instead_of_credential_names(
) -> Result<(), SipError> {
    let (engine, output) = register("permitted", 1, true)?;
    assert_eq!(status(&output)?, 200);
    assert_eq!(
        engine.registered_peer("phone", 2),
        Some("127.0.0.1:5061".parse()?)
    );
    for target in ["device", "unknown", ""] {
        let (engine, output) = register(target, 1, true)?;
        assert_eq!(status(&output)?, 404, "{target}");
        assert!(output.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 2), None);
    }
    Ok(())
}

#[test]
fn disabled_or_unconfigured_addresses_cannot_acquire_dynamic_bindings() -> Result<(), SipError> {
    for (target, limit, configured) in [("permitted", 0, true), ("device", 1, false)] {
        let (engine, output) = register(target, limit, configured)?;
        assert_eq!(status(&output)?, 403);
        assert!(output.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 2), None);
    }
    Ok(())
}
