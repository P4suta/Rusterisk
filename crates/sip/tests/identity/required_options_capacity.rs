use super::{
    answered, authenticated_invite_with_headers, config, incoming_request, outbound, request,
};
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_sip::{Engine, SipError, SipEvent, MAX_MESSAGE};

#[test]
fn required_header_expansion_returns_a_cached_small_response_and_preserves_another_call(
) -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    engine.receive(&answered(&invite)?, peer, 0)?;
    let session = engine
        .active_session("outgoing", 7)
        .ok_or(SipError::InvalidPacket)?
        .clone();
    let required = format!("Require: {}x\r\n", "x,".repeat(5400));
    let unsupported = format!("Unsupported: {}x\r\n", "x, ".repeat(5400));
    assert_eq!(unsupported.len(), 16216);
    assert!(unsupported.len() <= MAX_MESSAGE);
    let (rejected, wire, _) = authenticated_invite_with_headers(&mut engine, &required)?;
    assert!(wire.len() <= MAX_MESSAGE);
    assert!(rejected.events.is_empty());
    assert_eq!(rejected.datagrams.len(), 1);
    let bytes = &rejected.datagrams[0].bytes;
    assert!(bytes.len() <= MAX_MESSAGE);
    let tail = bytes
        .strip_prefix(b"SIP/2.0 513 Message Too Large\r\n")
        .ok_or(SipError::InvalidPacket)?;
    let expanded = tail
        .len()
        .checked_add(b"SIP/2.0 420 Bad Extension\r\n".len())
        .and_then(|length| length.checked_add(unsupported.len()))
        .ok_or(SipError::Capacity)?;
    assert!(expanded > MAX_MESSAGE);
    let SipMessage::Response(response) = SipMessage::try_from(bytes.as_slice())? else {
        return Err(SipError::InvalidPacket);
    };
    assert!(!response
        .headers
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("Unsupported")));
    let repeated = wire.replace(";branch=z9hG4bK-2;rport", ";rport;branch=z9hG4bK-2");
    let replayed = engine.receive(repeated.as_bytes(), peer, 2)?;
    assert_eq!(replayed.datagrams[0].bytes, *bytes);
    assert!(replayed.events.is_empty());
    assert_eq!(engine.tick(501)?.datagrams[0].bytes, *bytes);
    let ack = incoming_request(bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-2", "z9hG4bK-2");
    let acknowledged = engine.receive(ack.as_bytes(), peer, 502)?;
    assert!(acknowledged.datagrams.is_empty());
    assert!(acknowledged.events.is_empty());
    assert!(engine.tick(1500)?.datagrams.is_empty());
    assert_eq!(engine.active_session("outgoing", 7), Some(&session));
    let invite = request(&invite)?;
    let bye = format!("BYE sip:rusterisk@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-after-oversized-require\r\nFrom: {};tag=callee\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value());
    let ended = engine.receive(bye.as_bytes(), peer, 1501)?;
    assert!(ended.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id, generation: 7, .. }] if id == "outgoing")
    );
    Ok(())
}
