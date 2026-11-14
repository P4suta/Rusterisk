use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    Method, Uri,
};
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_config::Config;
use rusterisk_sip::{Engine, Session, SipError, SipEvent};

fn config() -> Result<Config, SipError> {
    Ok(Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[a]\ntype=auth\nusername=phone\npassword=password\n[aor]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=a\naors=aor\ncontext=internal\nallow=ulaw,alaw,opus\n", "", "")?)
}

fn invite(authorization: &str, sequence: u32) -> String {
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 8 101\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{sequence};rport\r\nFrom: Admin <sip:spoofed@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: test-call\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn authenticated_invite(
    engine: &mut Engine,
) -> Result<(rusterisk_sip::Output, String, String), SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let challenged = engine.receive(invite("", 1).as_bytes(), peer, 0)?;
    assert!(challenged.events.is_empty());
    let response = std::str::from_utf8(&challenged.datagrams[0].bytes)?;
    assert!(response.starts_with("SIP/2.0 401"));
    let challenge = response
        .lines()
        .find_map(|line| line.strip_prefix("WWW-Authenticate: "))
        .ok_or(SipError::InvalidPacket)?;
    let challenge = WwwAuthenticate::parse(challenge)?;
    let ack = incoming_request(&challenged.datagrams[0].bytes, "ACK", 1)?;
    let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-1", "z9hG4bK-1");
    engine.receive(ack.as_bytes(), peer, 0)?;
    let uri = Uri::try_from("sip:100@localhost")?;
    let qop = AuthQop::Auth {
        cnonce: "test-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let auth = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"test-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    let wire = invite(&auth, 2);
    let accepted = engine.receive(wire.as_bytes(), peer, 1)?;
    Ok((accepted, wire, auth))
}

#[test]
fn authenticated_identity_is_distinct_from_caller_id_and_retransmission_is_idempotent(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (accepted, wire, auth) = authenticated_invite(&mut engine)?;
    assert_eq!(accepted.events.len(), 1);
    assert!(
        matches!(&accepted.events[0], SipEvent::Incoming { identity, caller, context, .. } if identity == "phone" && caller == "spoofed" && context == "internal")
    );
    let replay = engine.receive(wire.as_bytes(), peer, 2)?;
    assert!(replay.events.is_empty());
    assert_eq!(accepted.datagrams[0].bytes, replay.datagrams[0].bytes);
    assert!(engine
        .receive(invite(&auth, 3).as_bytes(), peer, 3)?
        .events
        .is_empty());
    Ok(())
}

#[test]
fn malformed_or_oversized_signaling_and_unsupported_audio_are_refused() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    assert!(engine
        .receive(&[0xff; 10], "127.0.0.1:5061".parse()?, 0)
        .is_err());
    assert!(engine
        .receive(&[0; 16385], "127.0.0.1:5061".parse()?, 0)
        .is_err());
    assert!(Session::parse(
        "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 9000 RTP/AVP 18\r\n",
        &[rusterisk_config::Codec::Pcmu]
    )
    .is_err());
    Ok(())
}

fn answered(invite: &[u8]) -> Result<Vec<u8>, SipError> {
    let request = match SipMessage::try_from(invite)? {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(SipError::InvalidPacket),
    };
    let body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 10002 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
    Ok(format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=callee\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value(), body.len()).into_bytes())
}

fn provisional(invite: &[u8]) -> Result<Vec<u8>, SipError> {
    let request = request(invite)?;
    Ok(format!("SIP/2.0 180 Ringing\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=callee\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value()).into_bytes())
}

#[test]
fn cancellation_waits_for_a_provisional_response_and_never_announces_a_late_answer(
) -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    assert!(engine.cancel("outgoing", 7, 1)?.datagrams.is_empty());
    assert!(engine.tick(500)?.datagrams[0].bytes.starts_with(b"INVITE "));
    let cancelled = engine.receive(&provisional(&invite)?, "127.0.0.1:5061".parse()?, 600)?;
    assert!(cancelled.datagrams[0].bytes.starts_with(b"CANCEL "));
    assert!(cancelled.events.is_empty());
    let late = engine.receive(&answered(&invite)?, "127.0.0.1:5061".parse()?, 700)?;
    assert_eq!(late.datagrams.len(), 2);
    assert!(late.events.is_empty());
    Ok(())
}

#[test]
fn a_cancelled_generation_does_not_prevent_reusing_the_channel_identifier() -> Result<(), SipError>
{
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    engine.originate(outbound(), 0)?;
    engine.cancel("outgoing", 7, 1)?;
    let mut replacement = outbound();
    replacement.generation = 8;
    assert!(engine.originate(replacement, 2)?.datagrams[0]
        .bytes
        .starts_with(b"INVITE "));
    assert!(engine.cancel("outgoing", 7, 3)?.datagrams.is_empty());
    Ok(())
}

fn outbound() -> rusterisk_sip::Outbound {
    rusterisk_sip::Outbound {
        id: "outgoing".into(),
        generation: 7,
        endpoint: "phone".into(),
        caller: "100".into(),
        local_rtp: std::net::SocketAddr::from(([127, 0, 0, 1], 10000)),
        timeout_ms: 30_000,
    }
}

#[test]
fn outgoing_invite_retransmits_and_late_answer_after_cancel_is_acknowledged_then_terminated(
) -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let output = engine.originate(outbound(), 0)?;
    assert!(output.datagrams[0].bytes.starts_with(b"INVITE "));
    assert!(engine.tick(499)?.datagrams.is_empty());
    assert_eq!(
        engine.tick(500)?.datagrams[0].bytes,
        output.datagrams[0].bytes
    );
    engine.receive(
        &provisional(&output.datagrams[0].bytes)?,
        "127.0.0.1:5061".parse()?,
        550,
    )?;
    let cancelled = engine.cancel("outgoing", 7, 600)?;
    assert!(cancelled
        .datagrams
        .iter()
        .any(|packet| packet.bytes.starts_with(b"CANCEL ")));
    let late = engine.receive(
        &answered(&output.datagrams[0].bytes)?,
        "127.0.0.1:5061".parse()?,
        700,
    )?;
    assert!(late
        .datagrams
        .iter()
        .any(|packet| packet.bytes.starts_with(b"ACK ")));
    assert!(late
        .datagrams
        .iter()
        .any(|packet| packet.bytes.starts_with(b"BYE ")));
    assert!(late.events.is_empty());
    Ok(())
}

#[test]
fn an_answer_from_the_wrong_peer_is_not_a_call_milestone() -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let output = engine.originate(outbound(), 0)?;
    assert!(engine
        .receive(
            &answered(&output.datagrams[0].bytes)?,
            "127.0.0.1:5999".parse()?,
            1
        )?
        .events
        .is_empty());
    let answer = engine.receive(
        &answered(&output.datagrams[0].bytes)?,
        "127.0.0.1:5061".parse()?,
        2,
    )?;
    assert!(answer.events.iter().any(
        |event| matches!(event, SipEvent::Answered { id, generation: 7, .. } if id == "outgoing")
    ));
    Ok(())
}

fn request(bytes: &[u8]) -> Result<rsipstack::sip::Request, SipError> {
    match SipMessage::try_from(bytes)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(SipError::InvalidPacket),
    }
}

#[test]
fn successful_ack_and_bye_have_distinct_transaction_branches() -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let invite_branch = request(&invite)?
        .top_via_header()?
        .typed()?
        .branch()
        .cloned();
    let answer = engine.receive(&answered(&invite)?, "127.0.0.1:5061".parse()?, 1)?;
    let ack = request(&answer.datagrams[0].bytes)?;
    assert_ne!(
        ack.top_via_header()?.typed()?.branch(),
        invite_branch.as_ref()
    );
    let bye = engine.cancel("outgoing", 7, 2)?.datagrams.remove(0).bytes;
    assert_ne!(
        request(&bye)?.top_via_header()?.typed()?.branch(),
        ack.top_via_header()?.typed()?.branch()
    );
    Ok(())
}

#[test]
fn repeated_late_answers_keep_the_original_invite_sequence_in_ack() -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    engine.receive(&provisional(&invite)?, "127.0.0.1:5061".parse()?, 1)?;
    let cancelled = engine.cancel("outgoing", 7, 2)?;
    assert_eq!(
        request(&cancelled.datagrams[0].bytes)?
            .top_via_header()?
            .typed()?
            .branch(),
        request(&invite)?.top_via_header()?.typed()?.branch()
    );
    for now in [2, 3, 4] {
        let output = engine.receive(&answered(&invite)?, "127.0.0.1:5061".parse()?, now)?;
        let ack = request(&output.datagrams[0].bytes)?;
        assert_eq!(ack.method, Method::Ack);
        assert_eq!(ack.cseq_header()?.typed()?.seq, 1);
        assert!(output.events.is_empty());
    }
    Ok(())
}

#[test]
fn remote_hangup_requires_the_negotiated_peer_and_dialog_tags() -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let wire = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let invite = request(&wire)?;
    engine.receive(&answered(&wire)?, "127.0.0.1:5061".parse()?, 1)?;
    let bye = format!("BYE sip:rusterisk@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-remote-bye\r\nFrom: {};tag=callee\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value());
    for (packet, peer) in [
        (&bye, "127.0.0.1:5999"),
        (&bye.replace("tag=callee", "tag=intruder"), "127.0.0.1:5061"),
    ] {
        let rejected = engine.receive(packet.as_bytes(), peer.parse()?, 2)?;
        assert!(rejected.events.is_empty());
        assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 481"));
    }
    let ended = engine.receive(bye.as_bytes(), "127.0.0.1:5061".parse()?, 3)?;
    assert!(ended.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(matches!(
        ended.events.as_slice(),
        [SipEvent::Ended {
            generation: 7,
            cause: 16,
            ..
        }]
    ));
    let repeated = engine.receive(bye.as_bytes(), "127.0.0.1:5061".parse()?, 4)?;
    assert!(repeated.events.is_empty());
    assert_eq!(repeated.datagrams[0].bytes, ended.datagrams[0].bytes);
    Ok(())
}

#[test]
fn acknowledgements_never_receive_a_response() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [9; 32]);
    let ack = invite("", 1).replace("INVITE", "ACK");
    assert!(engine
        .receive(ack.as_bytes(), "127.0.0.1:5061".parse()?, 0)?
        .datagrams
        .is_empty());
    Ok(())
}

#[test]
fn rejected_invites_release_admission_and_retransmit_the_tagged_final_response(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = match &incoming.events[0] {
        SipEvent::Incoming { id, .. } => id.clone(),
        SipEvent::DtmfReceived { .. }
        | SipEvent::Ringing { .. }
        | SipEvent::Answered { .. }
        | SipEvent::Ended { .. }
        | SipEvent::IncomingEnded { .. } => return Err(SipError::InvalidPacket),
    };
    let rejected = engine.reject(&id, rusterisk_sip::RejectReason::NoRoute, 2)?;
    assert!(engine.pending(&id).is_none());
    let response = match SipMessage::try_from(rejected.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(response) => response,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    assert_eq!(response.status_code.code(), 404);
    assert!(response.to_header()?.typed()?.tag().is_some());
    assert_eq!(
        engine
            .receive(wire.as_bytes(), "127.0.0.1:5061".parse()?, 3)?
            .datagrams[0]
            .bytes,
        rejected.datagrams[0].bytes
    );
    assert_eq!(
        engine.tick(502)?.datagrams[0].bytes,
        rejected.datagrams[0].bytes
    );
    let ack = incoming_request(&rejected.datagrams[0].bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-2", "z9hG4bK-2");
    engine.receive(ack.as_bytes(), "127.0.0.1:5061".parse()?, 503)?;
    assert!(engine.tick(1502)?.datagrams.is_empty());
    Ok(())
}

fn incoming_id(output: &rusterisk_sip::Output) -> Result<String, SipError> {
    match output.events.first() {
        Some(SipEvent::Incoming { id, .. }) => Ok(id.clone()),
        Some(
            SipEvent::DtmfReceived { .. }
            | SipEvent::Ringing { .. }
            | SipEvent::Answered { .. }
            | SipEvent::Ended { .. }
            | SipEvent::IncomingEnded { .. },
        )
        | None => Err(SipError::InvalidPacket),
    }
}

fn incoming_request(answer: &[u8], method: &str, sequence: u32) -> Result<Vec<u8>, SipError> {
    let reply = match SipMessage::try_from(answer)? {
        SipMessage::Response(response) => response,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    Ok(format!("{method} sip:100@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-{method}-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", reply.from_header()?.value(), reply.to_header()?.value(), reply.call_id_header()?.value()).into_bytes())
}

#[test]
fn incoming_answers_retransmit_until_the_matching_ack_and_remote_hangup_is_idempotent(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let ringing = engine.ring(&id)?;
    assert!(ringing.datagrams[0].bytes.starts_with(b"SIP/2.0 180"));
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 3)?.datagrams[0].bytes,
        ringing.datagrams[0].bytes
    );
    let answer = engine.answer(&id, 9, "127.0.0.1:10000".parse()?, 10)?;
    assert!(matches!(
        &answer.events[0],
        SipEvent::Answered { generation: 9, .. }
    ));
    assert!(engine.pending(&id).is_none());
    assert!(engine.active_session(&id, 9).is_some());
    assert!(engine.tick(509)?.datagrams.is_empty());
    assert_eq!(
        engine.tick(510)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    let ack = incoming_request(&answer.datagrams[0].bytes, "ACK", 2)?;
    engine.receive(&ack, "127.0.0.1:5062".parse()?, 511)?;
    assert_eq!(
        engine.tick(1510)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    let wrong_sequence = incoming_request(&answer.datagrams[0].bytes, "ACK", 3)?;
    engine.receive(&wrong_sequence, peer, 1511)?;
    assert_eq!(
        engine.tick(3510)?.datagrams[0].bytes,
        answer.datagrams[0].bytes
    );
    assert!(engine.receive(&ack, peer, 3511)?.datagrams.is_empty());
    assert!(engine.tick(7510)?.datagrams.is_empty());
    let bye = incoming_request(&answer.datagrams[0].bytes, "BYE", 3)?;
    assert!(engine
        .receive(&bye, "127.0.0.1:5062".parse()?, 7511)?
        .datagrams[0]
        .bytes
        .starts_with(b"SIP/2.0 481"));
    let ended = engine.receive(&bye, peer, 7512)?;
    assert!(matches!(
        &ended.events[0],
        SipEvent::Ended {
            generation: 9,
            cause: 16,
            ..
        }
    ));
    assert!(ended.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(engine.active_session(&id, 9).is_none());
    assert!(engine.receive(&bye, peer, 7513)?.events.is_empty());
    Ok(())
}

#[test]
fn an_unacknowledged_incoming_answer_expires_and_terminates_the_peer() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let (incoming, _, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    engine.answer(&id, 9, "127.0.0.1:10000".parse()?, 10)?;
    let expired = engine.tick(32010)?;
    assert!(matches!(
        &expired.events[0],
        SipEvent::Ended {
            generation: 9,
            cause: 102,
            ..
        }
    ));
    assert!(expired.datagrams[0].bytes.starts_with(b"BYE "));
    assert!(engine.active_session(&id, 9).is_none());
    assert!(engine.tick(32011)?.events.is_empty());
    Ok(())
}

#[test]
fn cancellation_requires_the_original_invite_transaction_and_releases_pending_admission(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let cancel = wire.replace("INVITE", "CANCEL");
    let wrong_branch = cancel.replace("z9hG4bK-2", "z9hG4bK-other");
    for (packet, address) in [
        (wrong_branch.as_str(), peer),
        (cancel.as_str(), "127.0.0.1:5062".parse()?),
    ] {
        assert!(engine.receive(packet.as_bytes(), address, 2)?.datagrams[0]
            .bytes
            .starts_with(b"SIP/2.0 481"));
        assert!(engine.pending(&id).is_some());
    }
    let cancelled = engine.receive(cancel.as_bytes(), peer, 3)?;
    assert_eq!(cancelled.datagrams.len(), 2);
    assert!(cancelled.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(cancelled.datagrams[1].bytes.starts_with(b"SIP/2.0 487"));
    assert_eq!(cancelled.events.len(), 1);
    assert!(engine.pending(&id).is_none());
    assert!(engine
        .answer(&id, 9, "127.0.0.1:10000".parse()?, 4)
        .is_err());
    let repeated = engine.receive(cancel.as_bytes(), peer, 5)?;
    assert_eq!(repeated.datagrams[0].bytes, cancelled.datagrams[0].bytes);
    assert!(repeated.events.is_empty());
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 6)?.datagrams[0].bytes,
        cancelled.datagrams[1].bytes
    );
    Ok(())
}

#[test]
fn authentication_challenges_retransmit_until_the_original_transaction_is_acknowledged(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let challenge = engine.receive(invite("", 1).as_bytes(), peer, 0)?;
    assert!(engine.tick(499)?.datagrams.is_empty());
    assert_eq!(
        engine.tick(500)?.datagrams[0].bytes,
        challenge.datagrams[0].bytes
    );
    let wrong_ack = incoming_request(&challenge.datagrams[0].bytes, "ACK", 1)?;
    engine.receive(&wrong_ack, peer, 501)?;
    assert_eq!(
        engine.tick(1500)?.datagrams[0].bytes,
        challenge.datagrams[0].bytes
    );
    let ack = std::str::from_utf8(&wrong_ack)?.replace("z9hG4bK-ACK-1", "z9hG4bK-1");
    engine.receive(ack.as_bytes(), peer, 1501)?;
    assert!(engine.tick(3500)?.datagrams.is_empty());
    let mut expired = Engine::new(config()?, [7; 32]);
    expired.receive(invite("", 1).as_bytes(), peer, 0)?;
    assert!(expired.tick(32000)?.datagrams.is_empty());
    Ok(())
}

fn challenge_invite(bytes: &[u8], proxy: bool) -> Result<Vec<u8>, SipError> {
    let invite = request(bytes)?;
    let (code, phrase, header) = if proxy {
        (407, "Proxy Authentication Required", "Proxy-Authenticate")
    } else {
        (401, "Unauthorized", "WWW-Authenticate")
    };
    Ok(format!("SIP/2.0 {code} {phrase}\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=challenge\r\nCall-ID: {}\r\nCSeq: {}\r\n{header}: Digest realm=\"rusterisk\", nonce=\"remote-nonce\", algorithm=MD5, qop=\"auth\", opaque=\"roundtrip\"\r\nContent-Length: 0\r\n\r\n", invite.top_via_header()?.value(), invite.from_header()?.value(), invite.to_header()?.value(), invite.call_id_header()?.value(), invite.cseq_header()?.value()).into_bytes())
}

fn authorization(request: &rsipstack::sip::Request, proxy: bool) -> Option<&str> {
    let name = if proxy {
        "Proxy-Authorization"
    } else {
        "Authorization"
    };
    request
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .map(|header| header.value())
}

#[test]
fn outgoing_authentication_preserves_caller_identity_and_uses_a_fresh_transaction(
) -> Result<(), SipError> {
    for proxy in [false, true] {
        let mut config = config()?;
        config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
        config.endpoints[0].outbound_auth.push("a".into());
        let mut engine = Engine::new(config, [9; 32]);
        let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
        let challenged = engine.receive(
            &challenge_invite(&invite, proxy)?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        assert_eq!(challenged.datagrams.len(), 2);
        assert!(challenged.datagrams[0].bytes.starts_with(b"ACK "));
        assert!(challenged.events.is_empty());
        let replayed = engine.receive(
            &challenge_invite(&invite, proxy)?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        assert_eq!(replayed.datagrams.len(), 1);
        assert_eq!(replayed.datagrams[0].bytes, challenged.datagrams[0].bytes);
        let authorized = request(&challenged.datagrams[1].bytes)?;
        assert_eq!(authorized.cseq_header()?.value(), "2 INVITE");
        assert_eq!(
            authorized.from_header()?.value(),
            request(&invite)?.from_header()?.value()
        );
        assert_ne!(
            authorized.top_via_header()?.value(),
            request(&invite)?.top_via_header()?.value()
        );
        let authorization = authorization(&authorized, proxy).ok_or(SipError::InvalidPacket)?;
        let authorization = rsipstack::sip::headers::typed::Authorization::parse(authorization)?;
        assert_eq!(authorization.username, "phone");
        assert_eq!(authorization.opaque.as_deref(), Some("roundtrip"));
        assert_eq!(
            authorization.response,
            DigestGenerator::from(&authorization, "password", &Method::Invite).compute()
        );
        let answer = engine.receive(
            &answered(&challenged.datagrams[1].bytes)?,
            "127.0.0.1:5061".parse()?,
            2,
        )?;
        assert!(matches!(
            &answer.events[0],
            SipEvent::Answered { generation: 7, .. }
        ));
        let ack = request(&answer.datagrams[0].bytes)?;
        assert!(self::authorization(&ack, proxy).is_some());
    }
    Ok(())
}

#[test]
fn rejected_or_unconfigured_outbound_credentials_are_never_retried() -> Result<(), SipError> {
    for (configured, matching_realm) in [(false, true), (true, false), (true, true)] {
        let mut config = config()?;
        config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
        if configured {
            config.endpoints[0].outbound_auth.push("a".into());
        }
        if !matching_realm {
            config.auth[0].realm = "another-peer".into();
        }
        let mut engine = Engine::new(config, [9; 32]);
        let initial = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
        let mut reply = engine.receive(
            &challenge_invite(&initial, false)?,
            "127.0.0.1:5061".parse()?,
            1,
        )?;
        if configured && matching_realm {
            reply = engine.receive(
                &challenge_invite(&reply.datagrams[1].bytes, false)?,
                "127.0.0.1:5061".parse()?,
                2,
            )?;
        }
        assert_eq!(reply.datagrams.len(), 1);
        assert!(reply.datagrams[0].bytes.starts_with(b"ACK "));
        assert!(matches!(
            &reply.events[0],
            SipEvent::Ended { cause: 21, .. }
        ));
    }
    Ok(())
}

fn transaction_response(bytes: &[u8], code: u16) -> Result<Vec<u8>, SipError> {
    let request = request(bytes)?;
    Ok(format!("SIP/2.0 {code} Response\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value()).into_bytes())
}

#[test]
fn hangup_retries_until_a_matching_response_and_ignores_other_peers() -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    engine.receive(&answered(&invite)?, peer, 1)?;
    let bye = engine.cancel("outgoing", 7, 2)?.datagrams.remove(0).bytes;
    assert!(engine.tick(501)?.datagrams.is_empty());
    assert_eq!(engine.tick(502)?.datagrams[0].bytes, bye);
    let response = transaction_response(&bye, 200)?;
    engine.receive(&response, "127.0.0.1:5062".parse()?, 503)?;
    assert_eq!(engine.tick(1502)?.datagrams[0].bytes, bye);
    assert!(engine.receive(&response, peer, 1503)?.events.is_empty());
    assert!(engine.tick(3502)?.datagrams.is_empty());
    assert!(engine.active_session("outgoing", 7).is_none());
    Ok(())
}

#[test]
fn cancellation_retries_with_a_bounded_interval_and_expires_without_a_response(
) -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    engine.receive(&provisional(&invite)?, peer, 1)?;
    let cancel = engine.cancel("outgoing", 7, 2)?.datagrams.remove(0).bytes;
    assert_eq!(engine.tick(502)?.datagrams[0].bytes, cancel);
    engine.receive(&transaction_response(&cancel, 100)?, peer, 503)?;
    assert!(engine.tick(1501)?.datagrams.is_empty());
    assert_eq!(engine.tick(1502)?.datagrams[0].bytes, cancel);
    assert!(engine.tick(5501)?.datagrams.is_empty());
    assert_eq!(engine.tick(5502)?.datagrams[0].bytes, cancel);
    assert!(engine.tick(32002)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn pending_invitations_keep_their_last_response_until_a_final_decision() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let ringing = engine.ring(&id)?;
    assert!(engine.tick(32003)?.datagrams.is_empty());
    let repeated = engine.receive(wire.as_bytes(), peer, 32004)?;
    assert!(repeated.events.is_empty());
    assert_eq!(repeated.datagrams[0].bytes, ringing.datagrams[0].bytes);
    engine.reject(&id, rusterisk_sip::RejectReason::NoRoute, 32005)?;
    assert!(engine.tick(64005)?.datagrams.is_empty());
    assert!(engine.receive(wire.as_bytes(), peer, 64006)?.datagrams[0]
        .bytes
        .starts_with(b"SIP/2.0 401"));
    Ok(())
}

#[test]
fn incoming_info_reports_each_valid_digit_once_and_checks_dialog_ownership() -> Result<(), SipError>
{
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, _, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let answer = engine.answer(&id, 9, "127.0.0.1:10000".parse()?, 10)?;
    let info = incoming_request(&answer.datagrams[0].bytes, "INFO", 3)?;
    let info = std::str::from_utf8(&info)?.replace("Content-Length: 0\r\n\r\n", "Content-Type: application/dtmf-relay\r\nContent-Length: 24\r\n\r\nSignal=5\r\nDuration=100\r\n");
    let refused = engine.receive(info.as_bytes(), "127.0.0.1:5062".parse()?, 11)?;
    assert!(refused.events.is_empty());
    assert!(refused.datagrams[0].bytes.starts_with(b"SIP/2.0 481"));
    let received = engine.receive(info.as_bytes(), peer, 12)?;
    assert!(received.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(matches!(
        &received.events[0],
        SipEvent::DtmfReceived {
            generation: 9,
            digit: rusterisk_core::DtmfDigit::Five,
            duration_ms: 100,
            ..
        }
    ));
    assert!(engine.receive(info.as_bytes(), peer, 13)?.events.is_empty());
    Ok(())
}

#[test]
fn empty_info_is_acknowledged_without_a_body_type_and_unknown_bodies_are_refused(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, _, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let answer = engine.answer(&id, 9, "127.0.0.1:10000".parse()?, 10)?;
    let empty = incoming_request(&answer.datagrams[0].bytes, "INFO", 3)?;
    let received = engine.receive(&empty, peer, 11)?;
    assert!(received.events.is_empty());
    assert!(received.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    let unknown = incoming_request(&answer.datagrams[0].bytes, "INFO", 4)?;
    let unknown = std::str::from_utf8(&unknown)?.replace(
        "Content-Length: 0\r\n\r\n",
        "Content-Type: text/plain\r\nContent-Length: 7\r\n\r\nunknown",
    );
    let refused = engine.receive(unknown.as_bytes(), peer, 12)?;
    assert!(refused.events.is_empty());
    assert!(refused.datagrams[0].bytes.starts_with(b"SIP/2.0 415"));
    assert!(engine.active_session(&id, 9).is_some());
    Ok(())
}
