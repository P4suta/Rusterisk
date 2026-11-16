use super::configuration;
use md5::Md5;
use rsipstack::sip::{headers::typed::WwwAuthenticate, HeadersExt, Response, SipMessage};
use rusterisk_config::Protocol;
use rusterisk_sip::{Engine, Output, SipError, SipEvent};
use sha2::Digest;

fn token(protocol: Protocol) -> Result<&'static str, SipError> {
    match protocol {
        Protocol::Tls => Ok("TLS"),
        Protocol::Wss => Ok("WSS"),
        Protocol::Udp | Protocol::Tcp | Protocol::Ws => Err(SipError::Unsupported),
    }
}

fn invite(
    uri: &str,
    contact: &str,
    transport: &str,
    sequence: u32,
    authorization: &str,
    to: &str,
    direction: &str,
) -> String {
    let body = format!("v=0\r\no=- 1 {sequence} IN IP4 127.0.0.2\r\ns=call\r\nc=IN IP4 127.0.0.2\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0\r\na={direction}\r\n");
    format!("INVITE {uri} SIP/2.0\r\nVia: SIP/2.0/{transport} 127.0.0.2:5070;branch=z9hG4bK-secure-{sequence};rport\r\nFrom: <sip:caller@provider.example>;tag=caller\r\nTo: {to}\r\nCall-ID: incoming-uri-security\r\nCSeq: {sequence} INVITE\r\nContact: <{contact}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn response(output: &Output) -> Result<Response, SipError> {
    let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    match SipMessage::try_from(packet.bytes.as_slice())? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn hash(source: &str) -> String {
    Md5::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn authorization(uri: &str, challenge: &WwwAuthenticate, count: u32) -> String {
    let a1 = hash(&format!("phone:{}:password", challenge.realm));
    let a2 = hash(&format!("INVITE:{uri}"));
    let digest = hash(&format!(
        "{a1}:{}:{count:08x}:address-client:auth:{a2}",
        challenge.nonce
    ));
    format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"address-client\", nc={count:08x}\r\n", challenge.realm, challenge.nonce)
}

fn dialog(protocol: Protocol, scheme: &str) -> Result<(), SipError> {
    let config = configuration(protocol, "sip:phone@127.0.0.2:5070")?;
    let local = config
        .transports
        .first()
        .ok_or(SipError::InvalidPacket)?
        .bind;
    let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
    let peer = "127.0.0.2:5070".parse()?;
    let parameter = match protocol {
        Protocol::Tls => "tcp",
        Protocol::Wss => "ws",
        Protocol::Udp | Protocol::Tcp | Protocol::Ws => return Err(SipError::Unsupported),
    };
    let transport = token(protocol)?;
    let uri = format!("{scheme}:100@{local}");
    let remote_contact = format!("{scheme}:phone@127.0.0.2:5070;transport={parameter}");
    let to = format!("<{uri}>");
    let first = engine.receive(
        invite(&uri, &remote_contact, transport, 1, "", &to, "sendrecv").as_bytes(),
        peer,
        0,
    )?;
    let rejected = response(&first)?;
    assert_eq!(rejected.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        rejected
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let ack = format!("ACK {uri} SIP/2.0\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: incoming-uri-security\r\nCSeq: 1 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", rejected.top_via_header()?.value(), rejected.from_header()?.value(), rejected.to_header()?.value());
    engine.receive(ack.as_bytes(), peer, 0)?;
    let accepted = engine.receive(
        invite(
            &uri,
            &remote_contact,
            transport,
            2,
            &authorization(&uri, &challenge, 1),
            &to,
            "sendrecv",
        )
        .as_bytes(),
        peer,
        1,
    )?;
    let id = accepted
        .events
        .iter()
        .find_map(|event| {
            if let SipEvent::Incoming { id, .. } = event {
                Some(id.as_str())
            } else {
                None
            }
        })
        .ok_or(SipError::InvalidPacket)?;
    let answered = engine.answer(id, 7, "127.0.0.1:10000".parse()?, 2)?;
    let answered = response(&answered)?;
    assert_eq!(answered.status_code.code(), 200);
    let expected = format!("<{scheme}:rusterisk@{local};transport={parameter}>");
    assert_eq!(answered.contact_header()?.value(), expected);
    let target = expected
        .strip_prefix('<')
        .and_then(|source| source.strip_suffix('>'))
        .ok_or(SipError::InvalidPacket)?;
    let ack = format!("ACK {target} SIP/2.0\r\nVia: SIP/2.0/{transport} 127.0.0.2:5070;branch=z9hG4bK-secure-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: incoming-uri-security\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", answered.from_header()?.value(), answered.to_header()?.value());
    engine.receive(ack.as_bytes(), peer, 3)?;
    let refreshed = engine.receive(
        invite(
            target,
            &remote_contact,
            transport,
            3,
            &authorization(target, &challenge, 2),
            answered.to_header()?.value(),
            "sendonly",
        )
        .as_bytes(),
        peer,
        4,
    )?;
    let refreshed = response(&refreshed)?;
    assert_eq!(refreshed.status_code.code(), 200);
    assert_eq!(refreshed.contact_header()?.value(), expected);
    Ok(())
}

#[test]
fn incoming_sip_dialogs_keep_sip_contacts_over_tls_and_secure_websockets() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        dialog(protocol, "sip")?;
    }
    Ok(())
}

#[test]
fn incoming_sips_dialogs_preserve_secure_contacts_during_target_refresh() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        dialog(protocol, "sips")?;
    }
    Ok(())
}

#[test]
fn a_sips_request_with_an_insecure_contact_is_a_bad_request() -> Result<(), SipError> {
    let config = configuration(Protocol::Tls, "sip:phone@127.0.0.2:5070")?;
    let mut engine = Engine::new(config, [7; 32]).with_transport(Protocol::Tls);
    let uri = "sips:100@127.0.0.1:5061";
    let output = engine.receive(
        invite(
            uri,
            "sip:phone@127.0.0.2:5070;transport=tcp",
            "TLS",
            1,
            "",
            &format!("<{uri}>"),
            "sendrecv",
        )
        .as_bytes(),
        "127.0.0.2:5070".parse()?,
        0,
    )?;
    assert_eq!(response(&output)?.status_code.code(), 400);
    assert!(output.events.is_empty());
    Ok(())
}
