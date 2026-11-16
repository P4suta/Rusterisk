use super::configuration;
use md5::Md5;
use rsipstack::sip::{
    headers::{typed::WwwAuthenticate, untyped::ToTypedHeader},
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_config::Protocol;
use rusterisk_sip::{Engine, Output, SipError, SipEvent};
use sha2::Digest;
use std::net::SocketAddr;

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

struct DialogFixture {
    engine: Engine,
    peer: SocketAddr,
    transport: &'static str,
    target: String,
    remote_contact: String,
    to: String,
    challenge: WwwAuthenticate,
    contact: String,
    id: String,
}

impl DialogFixture {
    fn refresh(
        &mut self,
        uri: &str,
        contact: &str,
        sequence: u32,
        count: u32,
        direction: &str,
        now: u64,
    ) -> Result<Output, SipError> {
        let packet = invite(
            uri,
            contact,
            self.transport,
            sequence,
            &authorization(uri, &self.challenge, count),
            &self.to,
            direction,
        )
        .replace(
            &format!("branch=z9hG4bK-secure-{sequence};"),
            &format!("branch=z9hG4bK-secure-{sequence}-{count};"),
        );
        self.engine.receive(packet.as_bytes(), self.peer, now)
    }
}

fn established(protocol: Protocol, scheme: &str) -> Result<DialogFixture, SipError> {
    established_with_contact(protocol, scheme, "")
}

fn established_with_contact(
    protocol: Protocol,
    scheme: &str,
    suffix: &str,
) -> Result<DialogFixture, SipError> {
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
    let remote_contact = format!("{scheme}:phone@127.0.0.2:5070;transport={parameter}{suffix}");
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
                Some(id.clone())
            } else {
                None
            }
        })
        .ok_or(SipError::InvalidPacket)?;
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 2)?;
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
    Ok(DialogFixture {
        engine,
        peer,
        transport,
        target: target.into(),
        remote_contact,
        to: answered.to_header()?.value().into(),
        challenge,
        contact: expected,
        id,
    })
}

fn dialog(protocol: Protocol, scheme: &str) -> Result<(), SipError> {
    let mut fixture = established(protocol, scheme)?;
    let target = fixture.target.clone();
    let contact = fixture.remote_contact.clone();
    let refreshed = fixture.refresh(&target, &contact, 3, 2, "sendonly", 4)?;
    let refreshed = response(&refreshed)?;
    assert_eq!(refreshed.status_code.code(), 200);
    assert_eq!(refreshed.contact_header()?.value(), fixture.contact);
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

#[test]
fn unusable_incoming_contacts_are_rejected_before_dialog_admission() -> Result<(), SipError> {
    for (protocol, contact) in [
        (Protocol::Tls, "sip:caller@127.0.0.2:5070;transport=tls"),
        (Protocol::Tls, "sip:caller@127.0.0.2:5070;%74ransport=tls"),
        (Protocol::Tls, "sip:caller@127.0.0.2:5070;transport=udp"),
        (Protocol::Wss, "sip:caller@127.0.0.2:5070;transport=tls"),
        (Protocol::Wss, "sip:caller@127.0.0.2:5070;transport=tcp"),
    ] {
        let config = configuration(protocol, "sip:phone@127.0.0.2:5070")?;
        let mut engine = Engine::new(config, [7; 32]).with_transport(protocol);
        let uri = "sip:100@127.0.0.1:5061";
        let output = engine.receive(
            invite(
                uri,
                contact,
                token(protocol)?,
                1,
                "",
                &format!("<{uri}>"),
                "sendrecv",
            )
            .as_bytes(),
            "127.0.0.2:5070".parse()?,
            0,
        )?;
        assert_eq!(response(&output)?.status_code.code(), 400, "{contact}");
        assert!(output.events.is_empty());
    }
    Ok(())
}

#[test]
fn rejected_secure_target_downgrades_preserve_media_and_remote_sequence() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        let mut fixture = established(protocol, "sips")?;
        let target = fixture.target.replacen("sips:", "sip:", 1);
        let contact = fixture.remote_contact.replacen("sips:", "sip:", 1);
        let before = fixture
            .engine
            .active_session(&fixture.id, 7)
            .ok_or(SipError::InvalidPacket)?
            .direction;
        let rejected = fixture.refresh(&target, &contact, 3, 2, "sendonly", 4)?;
        assert_eq!(response(&rejected)?.status_code.code(), 400);
        assert!(rejected.events.is_empty());
        assert_eq!(
            fixture
                .engine
                .active_session(&fixture.id, 7)
                .ok_or(SipError::InvalidPacket)?
                .direction,
            before
        );
        let target = fixture.target.clone();
        let contact = fixture.remote_contact.clone();
        let accepted = fixture.refresh(&target, &contact, 3, 3, "sendonly", 5)?;
        assert_eq!(response(&accepted)?.status_code.code(), 200);
        assert!(matches!(
            accepted.events.as_slice(),
            [SipEvent::PeerDirection {
                direction: rusterisk_sip::Direction::SendOnly,
                ..
            }]
        ));
    }
    Ok(())
}

#[test]
fn rejected_secure_target_downgrades_preserve_the_hangup_target() -> Result<(), SipError> {
    let mut fixture = established(Protocol::Tls, "sips")?;
    let target = fixture.target.replacen("sips:", "sip:", 1);
    let contact = fixture.remote_contact.replacen("sips:", "sip:", 1);
    let rejected = fixture.refresh(&target, &contact, 3, 2, "sendonly", 4)?;
    assert_eq!(response(&rejected)?.status_code.code(), 400);
    let output = fixture.engine.cancel(&fixture.id, 7, 5)?;
    let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    let SipMessage::Request(request) = SipMessage::try_from(packet.bytes.as_slice())? else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request.method, Method::Bye);
    assert_eq!(request.uri, Uri::try_from(fixture.remote_contact.as_str())?);
    Ok(())
}

#[test]
fn secure_refresh_targets_preserve_the_starting_response_contact_role() -> Result<(), SipError> {
    for protocol in [Protocol::Tls, Protocol::Wss] {
        let mut fixture = established(protocol, "sip")?;
        let target = fixture.target.replacen("sip:", "sips:", 1);
        let contact = fixture.remote_contact.replacen("sip:", "sips:", 1);
        let accepted = fixture.refresh(&target, &contact, 3, 2, "sendonly", 4)?;
        let accepted = response(&accepted)?;
        assert_eq!(accepted.status_code.code(), 200);
        assert_eq!(accepted.contact_header()?.value(), fixture.contact);
    }
    Ok(())
}

#[test]
fn disallowed_contact_components_cannot_change_generated_methods_or_headers() -> Result<(), SipError>
{
    for (protocol, parameter) in [(Protocol::Tls, "tcp"), (Protocol::Wss, "ws")] {
        for suffix in [
            ";lr;opaque=kept;method=OPTIONS?Subject=call&body=untrusted&CSeq=999%20OPTIONS",
            ";lr;opaque=kept;%6dethod=OPTIONS?Route=%3Csip%3Aevil.invalid%3E",
        ] {
            let mut fixture = established_with_contact(protocol, "sip", suffix)?;
            let output = fixture.engine.cancel(&fixture.id, 7, 4)?;
            let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
            let SipMessage::Request(request) = SipMessage::try_from(packet.bytes.as_slice())?
            else {
                return Err(SipError::InvalidPacket);
            };
            assert_eq!(request.method, Method::Bye);
            assert_eq!(
                request.uri,
                Uri::try_from(format!(
                    "sip:phone@127.0.0.2:5070;transport={parameter};lr;opaque=kept"
                ))?
            );
            assert!(request.body.is_empty());
            assert_ne!(request.cseq_header()?.typed()?.seq, 999);
            assert!(!request.headers.iter().any(|header| {
                header.name().eq_ignore_ascii_case("Subject")
                    || header.name().eq_ignore_ascii_case("Route")
            }));
        }
    }
    Ok(())
}
