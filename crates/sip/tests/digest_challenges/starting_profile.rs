use super::{attribute, authorization, authorized, challenge, request, sha256};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request};
use rusterisk_config::{Config, Protocol};
use rusterisk_sip::{
    Destination, Engine, Outbound, OutboundProfile, SipError, SipEvent, StaticTarget,
};

const TARGET: &str = "sip:starting-user@127.0.0.1:5061";
const CHANGED_ENDPOINT: &str = "[phone]\ntype=endpoint\ntransport=signaling\nauth=incoming\noutbound_auth=remote\naors=phone\ncontext=replacement-context\nallow=alaw\nallow_transfer=no\nrefer_blind_progress=no\n";

fn configuration(pjsip: &str) -> Result<Config, SipError> {
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        pjsip,
        "",
        "",
    )?)
}

fn starting_configuration() -> Result<Config, SipError> {
    configuration(&format!("[signaling]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[incoming]\ntype=auth\nusername=inbound-only\npassword=inbound-password\n[remote]\ntype=auth\nusername=starting-auth\npassword=starting-password\nrealm=provider.example\n[phone]\ntype=aor\ncontact={TARGET}\n[phone]\ntype=endpoint\ntransport=signaling\nauth=incoming\noutbound_auth=remote\naors=phone\ncontext=starting-context\nallow=ulaw\nallow_transfer=yes\nrefer_blind_progress=yes\n"))
}

fn replacement_configuration(endpoint: &str) -> Result<Config, SipError> {
    configuration(&format!("[signaling]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:5090\n[incoming]\ntype=auth\nusername=replacement-inbound\npassword=replacement-inbound-password\n[remote]\ntype=auth\nusername=replacement-auth\npassword=replacement-password\nrealm=provider.example\n[phone]\ntype=aor\ncontact=sip:replacement-user@127.0.0.1:5091\n{endpoint}"))
}

fn assert_starting_invite(invite: &Request) -> Result<(), SipError> {
    assert_eq!(invite.method, Method::Invite);
    assert_eq!(invite.uri.to_string(), TARGET);
    let via = invite.top_via_header()?.typed()?;
    assert_eq!(via.transport, rsipstack::sip::Transport::Udp);
    assert_eq!(via.sent_by().to_string(), "127.0.0.1:5060");
    assert!(via.branch().is_some());
    assert_eq!(
        invite.contact_header()?.value(),
        "<sip:rusterisk@127.0.0.1:5060>"
    );
    assert!(invite
        .from_header()?
        .value()
        .starts_with("<sip:displayed-caller@127.0.0.1>;tag="));
    let body = std::str::from_utf8(&invite.body)?;
    assert!(body.contains("m=audio 10000 RTP/AVP 0 101\r\n"));
    assert!(body.contains("a=rtpmap:0 PCMU/8000\r\n"));
    assert!(!body.contains("PCMA/8000"));
    Ok(())
}

fn assert_starting_auth(invite: &Request) -> Result<(), SipError> {
    let header = authorization(invite, false)?;
    assert_eq!(attribute(header, "username")?, "starting-auth");
    assert_eq!(attribute(header, "realm")?, "provider.example");
    assert_eq!(attribute(header, "algorithm")?, "SHA-256");
    assert_eq!(attribute(header, "qop")?, "auth");
    assert_eq!(attribute(header, "nc")?, "00000001");
    assert_eq!(attribute(header, "uri")?, TARGET);
    let identity = sha256("starting-auth:provider.example:starting-password");
    let target = sha256(format!("INVITE:{TARGET}"));
    let expected = sha256(format!(
        "{identity}:starting-nonce:00000001:{}:auth:{target}",
        attribute(header, "cnonce")?
    ));
    assert_eq!(attribute(header, "response")?, expected);
    Ok(())
}

fn assert_starting_authority(engine: &mut Engine, invite: &Request) -> Result<(), SipError> {
    let remote_to = format!("{};tag=callee", invite.to_header()?.value());
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
    let answer = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {remote_to}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value(), body.len());
    let answered = engine.receive(answer.as_bytes(), "127.0.0.1:5061".parse()?, 2)?;
    assert!(answered.events.iter().any(|event| matches!(
        event,
        SipEvent::Answered { id, generation: 1, .. } if id == "starting-profile"
    )));
    let ack = request(
        &answered
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?;
    assert_eq!(ack.method, Method::Ack);
    let target = invite.contact_header()?.typed()?.uri;
    let packet = format!("REFER {target} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-starting-refer;rport\r\nFrom: {remote_to}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\nRefer-To: <sip:200@target.invalid>\r\nContent-Length: 0\r\n\r\n", invite.from_header()?.value(), invite.call_id_header()?.value());
    let referred = engine.receive(packet.as_bytes(), "127.0.0.1:5061".parse()?, 3)?;
    let authority = referred
        .events
        .iter()
        .find_map(|event| {
            let SipEvent::BlindTransferRequested {
                id,
                generation: 1,
                refer,
            } = event
            else {
                return None;
            };
            (id == "starting-profile").then_some(refer.authority())
        })
        .ok_or(SipError::InvalidPacket)?;
    assert_eq!(authority.endpoint(), "phone");
    assert_eq!(authority.context(), "starting-context");
    assert_eq!(authority.authenticated_identity(), None);
    Ok(())
}

fn assert_captured_profile(replacement: Config) -> Result<(), SipError> {
    let mut starting = starting_configuration()?;
    starting.transports[0].bind.set_port(0);
    let profile = OutboundProfile::capture(&starting, "phone")?;
    let copied = profile
        .clone()
        .bind_address("127.0.0.1:0".parse()?, "127.0.0.1:5060".parse()?)?;
    let diagnostic = format!("{copied:?}");
    assert!(!diagnostic.contains("password"));
    assert!(!diagnostic.contains("starting-auth"));
    assert!(!diagnostic.contains("inbound-only"));
    drop(profile);
    let mut engine = Engine::new(replacement, [31; 32]).with_transport(Protocol::Udp);
    let prepared = engine.prepare_outbound(copied, 0)?;
    assert_eq!(
        prepared.destination(),
        &Destination::Static(StaticTarget::from_uri(Protocol::Udp, TARGET)?)
    );
    let initial = engine.originate_prepared(
        Outbound {
            id: "starting-profile".into(),
            generation: 1,
            endpoint: "phone".into(),
            caller: "displayed-caller".into(),
            local_rtp: "127.0.0.1:10000".parse()?,
        },
        prepared,
        None,
        None,
        0,
    )?;
    assert_eq!(initial.datagrams.len(), 1);
    let packet = initial.datagrams.first().ok_or(SipError::InvalidPacket)?;
    assert_eq!(packet.peer, "127.0.0.1:5061".parse()?);
    let invite = request(&packet.bytes)?;
    assert_starting_invite(&invite)?;
    assert!(invite.authorization_header().is_none());
    let retried = engine.receive(
        &challenge(
            &packet.bytes,
            false,
            &["Digest realm=\"provider.example\", nonce=\"starting-nonce\", algorithm=SHA-256, qop=\"auth\""],
        )?,
        packet.peer,
        1,
    )?;
    let retried = authorized(&retried)?;
    assert_starting_invite(&retried)?;
    assert_starting_auth(&retried)?;
    assert_starting_authority(&mut engine, &retried)
}

#[test]
fn bound_profile_addresses_refuse_mismatched_requests_and_unusable_ports() -> Result<(), SipError> {
    let mut starting = starting_configuration()?;
    starting.transports[0].bind.set_port(0);
    for (requested, bound) in [
        ("127.0.0.1:5060", "127.0.0.1:5060"),
        ("127.0.0.1:0", "127.0.0.1:0"),
        ("127.0.0.1:0", "127.0.0.2:5060"),
        ("127.0.0.1:0", "[::1]:5060"),
        ("127.0.0.1:0", "224.0.0.1:5060"),
    ] {
        let profile = OutboundProfile::capture(&starting, "phone")?;
        assert!(profile
            .bind_address(requested.parse()?, bound.parse()?)
            .is_err());
    }
    let profile = OutboundProfile::capture(&starting_configuration()?, "phone")?;
    assert!(profile
        .bind_address("127.0.0.1:5060".parse()?, "127.0.0.1:5061".parse()?)
        .is_err());
    starting.transports[0].bind = "0.0.0.0:0".parse()?;
    assert!(OutboundProfile::capture(&starting, "phone")?
        .bind_address("0.0.0.0:0".parse()?, "127.0.0.1:5060".parse()?)
        .is_ok());
    assert!(OutboundProfile::capture(&starting, "phone")?
        .bind_address("0.0.0.0:0".parse()?, "224.0.0.1:5060".parse()?)
        .is_err());
    Ok(())
}

#[test]
fn a_captured_outbound_profile_survives_endpoint_removal() -> Result<(), SipError> {
    assert_captured_profile(replacement_configuration("")?)
}

#[test]
fn a_captured_outbound_profile_ignores_same_name_replacements() -> Result<(), SipError> {
    assert_captured_profile(replacement_configuration(CHANGED_ENDPOINT)?)
}
