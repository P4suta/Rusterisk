use super::{authenticated_invite, config, incoming_id, incoming_request, request, response_tag};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Response, SipMessage};
use rusterisk_config::Codec;
use rusterisk_sip::{Engine, Output, Session, SipError, SipEvent};
use std::net::SocketAddr;

#[path = "early_media/provisional_lifetime.rs"]
mod provisional_lifetime;

fn response(output: &Output, code: u16) -> Result<Response, SipError> {
    assert_eq!(output.datagrams.len(), 1);
    assert_eq!(output.datagrams[0].peer, "127.0.0.1:5061".parse()?);
    let SipMessage::Response(response) =
        SipMessage::try_from(output.datagrams[0].bytes.as_slice())?
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), code);
    Ok(response)
}

fn cancellation(wire: &str) -> Result<String, SipError> {
    let invite = request(wire.as_bytes())?;
    Ok(format!(
        "CANCEL {} SIP/2.0\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {} CANCEL\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n",
        invite.uri,
        invite.top_via_header()?.value(),
        invite.from_header()?.value(),
        invite.to_header()?.value(),
        invite.call_id_header()?.value(),
        invite.cseq_header()?.typed()?.seq,
    ))
}

#[test]
fn progress_preserves_pending_ownership_and_replays_the_same_sdp_until_answer(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let local: SocketAddr = "127.0.0.1:10000".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let ringing = engine.ring(&id)?;
    let tag = response_tag(&ringing.datagrams[0].bytes)?.ok_or(SipError::InvalidPacket)?;
    let progress = engine.progress(&id, 9, local, 2)?;
    let provisional = response(&progress, 183)?;
    assert!(progress.events.is_empty());
    assert_eq!(
        response_tag(&progress.datagrams[0].bytes)?,
        Some(tag.clone())
    );
    let pending = engine.pending(&id).ok_or(SipError::InvalidPacket)?;
    assert_eq!(pending.local_tag, tag);
    assert!(engine.active_session(&id, 9).is_none());
    assert!(!provisional.body.is_empty());
    assert_eq!(
        Session::parse(std::str::from_utf8(&provisional.body)?, &[Codec::Pcmu])?.remote,
        local
    );
    let original = request(wire.as_bytes())?;
    assert_eq!(provisional.top_via_header()?, original.top_via_header()?);
    assert_eq!(provisional.from_header()?, original.from_header()?);
    assert_eq!(provisional.call_id_header()?, original.call_id_header()?);
    assert_eq!(provisional.cseq_header()?, original.cseq_header()?);
    assert_eq!(
        engine.progress(&id, 9, local, 3)?.datagrams[0].bytes,
        progress.datagrams[0].bytes
    );
    let repeated = engine.receive(wire.as_bytes(), peer, 4)?;
    assert_eq!(repeated.datagrams[0].bytes, progress.datagrams[0].bytes);
    assert!(repeated.events.is_empty());
    assert!(engine.tick(502)?.datagrams.is_empty());
    let answer = engine.answer(&id, 9, local, 503)?;
    let final_response = response(&answer, 200)?;
    assert_eq!(final_response.body, provisional.body);
    assert_eq!(final_response.to_header()?, provisional.to_header()?);
    assert_eq!(
        final_response.contact_header()?,
        provisional.contact_header()?
    );
    assert!(matches!(
        &answer.events[..],
        [SipEvent::Answered { id: actual, generation: 9, .. }] if actual == &id
    ));
    assert!(engine.pending(&id).is_none());
    assert!(engine.active_session(&id, 9).is_some());
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 504)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    assert_eq!(
        engine.tick(1003)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    let ack = incoming_request(&answer.datagrams[0].bytes, "ACK", 2)?;
    assert!(engine.receive(&ack, peer, 1004)?.events.is_empty());
    assert!(engine.tick(2003)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn cancellation_after_progress_retires_pending_media_and_never_revives_the_invitation(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let local: SocketAddr = "127.0.0.1:10000".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let progress = engine.progress(&id, 9, local, 2)?;
    let cancel = cancellation(&wire)?;
    let wrong_branch = cancel.replace("z9hG4bK-2", "z9hG4bK-another");
    for (packet, address) in [
        (wrong_branch.as_str(), peer),
        (cancel.as_str(), "127.0.0.1:5062".parse()?),
    ] {
        let rejected = engine.receive(packet.as_bytes(), address, 3)?;
        assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 481"));
        assert!(rejected.events.is_empty());
        assert!(engine.pending(&id).is_some());
    }
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 4)?.datagrams[0].bytes,
        progress.datagrams[0].bytes
    );
    let cancelled = engine.receive(cancel.as_bytes(), peer, 5)?;
    assert_eq!(cancelled.datagrams.len(), 2);
    assert!(cancelled.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(cancelled.datagrams[1].bytes.starts_with(b"SIP/2.0 487"));
    assert!(matches!(
        &cancelled.events[..],
        [SipEvent::IncomingEnded { id: actual, cause: 16 }] if actual == &id
    ));
    let tag = response_tag(&progress.datagrams[0].bytes)?;
    assert_eq!(response_tag(&cancelled.datagrams[0].bytes)?, tag);
    assert_eq!(response_tag(&cancelled.datagrams[1].bytes)?, tag);
    assert!(engine.pending(&id).is_none());
    assert!(engine.active_session(&id, 9).is_none());
    assert!(engine.progress(&id, 9, local, 6).is_err());
    assert!(engine.answer(&id, 9, local, 6).is_err());
    assert_eq!(
        engine.receive(cancel.as_bytes(), peer, 6)?.datagrams[0].bytes,
        cancelled.datagrams[0].bytes
    );
    let repeated = engine.receive(wire.as_bytes(), peer, 6)?;
    assert_eq!(repeated.datagrams[0].bytes, cancelled.datagrams[1].bytes);
    assert!(repeated.events.is_empty());
    let ack = incoming_request(&cancelled.datagrams[1].bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-2", "z9hG4bK-2");
    engine.receive(ack.as_bytes(), peer, 7)?;
    assert!(engine.tick(505)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn a_late_cancel_does_not_release_the_answered_early_dialog() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let local: SocketAddr = "127.0.0.1:10000".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    engine.progress(&id, 9, local, 2)?;
    let answer = engine.answer(&id, 9, local, 3)?;
    let cancelled = engine.receive(cancellation(&wire)?.as_bytes(), peer, 4)?;
    response(&cancelled, 200)?;
    assert!(cancelled.events.is_empty());
    assert!(engine.active_session(&id, 9).is_some());
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 5)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    let ack = incoming_request(&answer.datagrams[0].bytes, "ACK", 2)?;
    engine.receive(&ack, peer, 6)?;
    assert!(engine.tick(503)?.datagrams.is_empty());
    assert!(engine.active_session(&id, 9).is_some());
    Ok(())
}
