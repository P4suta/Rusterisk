use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::{Authorization, WwwAuthenticate},
        untyped::ToTypedHeader,
    },
    services::DigestGenerator,
    Method, Uri,
};
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_config::Config;
use rusterisk_sip::{Engine, Session, SipError, SipEvent};

#[path = "identity/early_media.rs"]
mod early_media;
#[path = "identity/flow_provenance.rs"]
mod flow_provenance;
#[path = "identity/required_options.rs"]
mod required_options;
#[path = "identity/required_options_capacity.rs"]
mod required_options_capacity;

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
    authenticated_invite_with_headers(engine, "")
}

fn authenticated_invite_with_headers(
    engine: &mut Engine,
    headers: &str,
) -> Result<(rusterisk_sip::Output, String, String), SipError> {
    let packet = |authorization, sequence| {
        invite(authorization, sequence).replacen(
            "Content-Type: ",
            &format!("{headers}Content-Type: "),
            1,
        )
    };
    let peer = "127.0.0.1:5061".parse()?;
    let challenged = engine.receive(packet("", 1).as_bytes(), peer, 0)?;
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
    let wire = packet(&auth, 2);
    let accepted = engine.receive(wire.as_bytes(), peer, 1)?;
    Ok((accepted, wire, auth))
}

fn reauthenticate(wire: &str, count: u32) -> Result<String, SipError> {
    let mut request = request(wire.as_bytes())?;
    let mut authorization = Authorization::parse(
        request
            .authorization_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    authorization.qop = Some(AuthQop::Auth {
        cnonce: "test-client".into(),
        nc: count,
    });
    authorization.response =
        DigestGenerator::from(&authorization, "password", &Method::Invite).compute();
    request
        .headers
        .unique_push(rsipstack::sip::Header::Authorization(
            authorization.to_string().into(),
        ));
    Ok(request.to_string())
}

#[test]
fn reordered_via_parameters_replay_the_same_transaction_without_another_call(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let repeated = wire.replace(";branch=z9hG4bK-2;rport", ";rport;branch=z9hG4bK-2");
    let replayed = engine.receive(repeated.as_bytes(), peer, 2)?;
    assert_eq!(replayed.datagrams[0].bytes, incoming.datagrams[0].bytes);
    assert!(replayed.events.is_empty());
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 3)?;
    let replayed = engine.receive(repeated.as_bytes(), peer, 4)?;
    assert_eq!(replayed.datagrams[0].bytes, answered.datagrams[0].bytes);
    assert!(replayed.events.is_empty());
    Ok(())
}

#[test]
fn method_case_does_not_alias_a_cached_invitation() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let changed = wire
        .replacen("INVITE", "invite", 1)
        .replace("CSeq: 2 INVITE", "CSeq: 2 invite");
    assert!(engine
        .receive(
            wire.replace("CSeq: 2 INVITE", "CSeq: 2 invite").as_bytes(),
            peer,
            2
        )
        .is_err());
    let unsupported = engine.receive(changed.as_bytes(), peer, 2)?;
    assert!(unsupported.datagrams[0].bytes.starts_with(b"SIP/2.0 501"));
    assert!(unsupported.events.is_empty());
    assert!(engine.pending(&id).is_some());
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 3)?.datagrams[0].bytes,
        incoming.datagrams[0].bytes
    );
    Ok(())
}

#[test]
fn cached_transactions_still_require_complete_request_headers() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    for header in ["From:", "To:", "Call-ID:", "CSeq:", "Via:", "Max-Forwards:"] {
        let malformed: String = wire
            .split_inclusive("\r\n")
            .filter(|line| !line.starts_with(header))
            .collect();
        assert!(
            engine.receive(malformed.as_bytes(), peer, 2).is_err(),
            "{header}"
        );
    }
    assert!(engine.pending(&id).is_some());
    assert_eq!(
        engine.receive(wire.as_bytes(), peer, 3)?.datagrams[0].bytes,
        incoming.datagrams[0].bytes
    );
    Ok(())
}

#[test]
fn cancellation_and_error_acknowledgement_use_the_original_transaction_identity(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let cancel = wire
        .replace("INVITE", "CANCEL")
        .replace(";branch=z9hG4bK-2;rport", ";rport;branch=z9hG4bK-2");
    let cancelled = engine.receive(cancel.as_bytes(), peer, 2)?;
    assert!(cancelled.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(cancelled.datagrams[1].bytes.starts_with(b"SIP/2.0 487"));
    assert!(
        matches!(&cancelled.events[..], [SipEvent::IncomingEnded { id: actual, .. }] if actual == &id)
    );
    assert!(engine.pending(&id).is_none());
    let ack = incoming_request(&cancelled.datagrams[1].bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?
        .replace("z9hG4bK-ACK-2", "z9hG4bK-2")
        .replace(";branch=z9hG4bK-2;rport", ";rport;branch=z9hG4bK-2");
    engine.receive(ack.as_bytes(), peer, 3)?;
    assert!(engine.tick(502)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn independent_dialogs_with_the_same_call_identifier_preserve_their_invitations_and_cleanup(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (original, wire, _) = authenticated_invite(&mut engine)?;
    let SipEvent::Incoming {
        id: original_id, ..
    } = &original.events[0]
    else {
        return Err(SipError::InvalidPacket);
    };
    let another = reauthenticate(
        &wire
            .replace("tag=caller", "tag=another")
            .replace("z9hG4bK-2", "z9hG4bK-another"),
        2,
    )?;
    let admitted = engine.receive(another.as_bytes(), peer, 2)?;
    let SipEvent::Incoming { id: another_id, .. } = &admitted.events[0] else {
        return Err(SipError::InvalidPacket);
    };
    assert_ne!(original_id, another_id);
    assert!(engine.pending(original_id).is_some());
    assert!(engine.pending(another_id).is_some());
    let original = engine.answer(original_id, 7, "127.0.0.1:10000".parse()?, 3)?;
    let another = engine.answer(another_id, 8, "127.0.0.1:10004".parse()?, 4)?;
    for answer in [&original, &another] {
        engine.receive(
            &incoming_request(&answer.datagrams[0].bytes, "ACK", 2)?,
            peer,
            5,
        )?;
    }
    assert!(engine.tick(510)?.datagrams.is_empty());
    let ended = engine.receive(
        &incoming_request(&original.datagrams[0].bytes, "BYE", 3)?,
        peer,
        511,
    )?;
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id, generation: 7, .. }] if id == original_id)
    );
    let bye = incoming_request(&another.datagrams[0].bytes, "BYE", 3)?;
    let bye = std::str::from_utf8(&bye)?.replace("z9hG4bK-BYE-3", "z9hG4bK-another-BYE-3");
    let ended = engine.receive(bye.as_bytes(), peer, 512)?;
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id, generation: 8, .. }] if id == another_id)
    );
    Ok(())
}

#[test]
fn merged_invitation_branches_do_not_create_or_replace_an_owned_call() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (original, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&original)?;
    let merged = reauthenticate(&wire.replace("z9hG4bK-2", "z9hG4bK-merged"), 2)?;
    let rejected = engine.receive(merged.as_bytes(), peer, 2)?;
    assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 482"));
    assert!(rejected.events.is_empty());
    assert!(engine.pending(&id).is_some());
    assert_eq!(
        engine.receive(merged.as_bytes(), peer, 3)?.datagrams[0].bytes,
        rejected.datagrams[0].bytes
    );
    engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 4)?;
    Ok(())
}

fn tagged_invitation(wire: &str, tag: &str, sequence: u32) -> Result<String, SipError> {
    let mut packet = request(wire.as_bytes())?;
    let to = packet.to_header()?.typed()?.with_tag(tag.into());
    packet.to_header_mut()?.replace(to.to_string());
    packet
        .cseq_header_mut()?
        .replace(format!("{sequence} INVITE"));
    reauthenticate(
        &packet
            .to_string()
            .replace("z9hG4bK-2", &format!("z9hG4bK-update-{sequence}")),
        2,
    )
}

fn direction_offer(
    wire: &str,
    answer: &[u8],
    sequence: u32,
    version: u64,
    direction: &str,
) -> Result<String, SipError> {
    let tag = response_tag(answer)?.ok_or(SipError::InvalidPacket)?;
    let offered = tagged_invitation(wire, &tag, sequence)?
        .replace("o=- 1 1 IN", &format!("o=- 1 {version} IN"))
        .replace("a=sendrecv", &format!("a={direction}"));
    let mut offered = request(offered.as_bytes())?;
    offered
        .headers
        .unique_push(rsipstack::sip::Header::ContentLength(
            offered.body.len().to_string().into(),
        ));
    Ok(offered.to_string())
}

#[test]
fn incoming_direction_updates_preserve_the_dialog_and_repeat_answers_until_ack(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 3)?;
    engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "ACK", 2)?,
        peer,
        4,
    )?;
    for (sequence, version, offered, expected, answer_direction) in [
        (
            3,
            2,
            "sendonly",
            rusterisk_sip::Direction::SendOnly,
            "recvonly",
        ),
        (
            4,
            3,
            "inactive",
            rusterisk_sip::Direction::Inactive,
            "inactive",
        ),
        (
            5,
            4,
            "sendrecv",
            rusterisk_sip::Direction::SendRecv,
            "sendrecv",
        ),
    ] {
        let offer = direction_offer(
            &wire,
            &answered.datagrams[0].bytes,
            sequence,
            version,
            offered,
        )?;
        let now = u64::from(sequence) * 1000;
        let updated = engine.receive(offer.as_bytes(), peer, now)?;
        assert!(updated.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
        let response = std::str::from_utf8(&updated.datagrams[0].bytes)?;
        assert!(response.contains(&format!("a={answer_direction}\r\n")));
        assert_eq!(
            engine
                .active_session(&id, 7)
                .ok_or(SipError::InvalidPacket)?
                .direction,
            expected
        );
        assert_eq!(updated.events.len(), 1);
        let repeated = engine.receive(offer.as_bytes(), peer, now + 1)?;
        assert_eq!(repeated.datagrams[0].bytes, updated.datagrams[0].bytes);
        assert!(repeated.events.is_empty());
        assert_eq!(
            engine.tick(now + 500)?.datagrams[0].bytes,
            updated.datagrams[0].bytes
        );
        engine.receive(
            &incoming_request(&updated.datagrams[0].bytes, "ACK", sequence)?,
            peer,
            now + 501,
        )?;
        assert!(engine.tick(now + 999)?.datagrams.is_empty());
    }
    let ended = engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "BYE", 6)?,
        peer,
        6000,
    )?;
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id: actual, generation: 7, .. }] if actual == &id)
    );
    Ok(())
}

#[test]
fn unsupported_direction_offers_leave_the_active_session_unchanged() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (incoming, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&incoming)?;
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 3)?;
    engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "ACK", 2)?,
        peer,
        4,
    )?;
    let original = engine
        .active_session(&id, 7)
        .ok_or(SipError::InvalidPacket)?
        .clone();
    for (sequence, version, replacement) in [
        (3, 1, None),
        (4, 2, Some(("10002", "10004"))),
        (5, 2, Some(("o=- 1", "o=- 2"))),
    ] {
        let mut offer = direction_offer(
            &wire,
            &answered.datagrams[0].bytes,
            sequence,
            version,
            "sendonly",
        )?;
        if let Some((from, to)) = replacement {
            offer = offer.replace(from, to);
        }
        let rejected = engine.receive(offer.as_bytes(), peer, u64::from(sequence) * 1000)?;
        assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 488"));
        assert!(rejected.events.is_empty());
        assert_eq!(engine.active_session(&id, 7), Some(&original));
        let repeated = engine.tick(u64::from(sequence) * 1000 + 500)?;
        assert_eq!(repeated.datagrams.len(), 1);
        assert_eq!(repeated.datagrams[0].bytes, rejected.datagrams[0].bytes);
        let ack = incoming_request(&rejected.datagrams[0].bytes, "ACK", sequence)?;
        let ack = std::str::from_utf8(&ack)?.replace(
            &format!("z9hG4bK-ACK-{sequence}"),
            &format!("z9hG4bK-update-{sequence}"),
        );
        engine.receive(ack.as_bytes(), peer, u64::from(sequence) * 1000 + 501)?;
        assert!(engine
            .tick(u64::from(sequence) * 1000 + 999)?
            .datagrams
            .is_empty());
    }
    Ok(())
}

#[test]
fn tagged_invitations_never_enter_initial_call_admission() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (original, wire, _) = authenticated_invite(&mut engine)?;
    let id = incoming_id(&original)?;
    let unknown = tagged_invitation(&wire, "unknown", 3)?;
    let rejected = engine.receive(unknown.as_bytes(), peer, 2)?;
    assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 481"));
    assert!(rejected.events.is_empty());
    assert!(engine.pending(&id).is_some());
    let answered = engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 3)?;
    engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "ACK", 2)?,
        peer,
        4,
    )?;
    let reply = match SipMessage::try_from(answered.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(reply) => reply,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    let tag = reply
        .to_header()?
        .typed()?
        .tag()
        .ok_or(SipError::InvalidPacket)?
        .value()
        .to_owned();
    let update = tagged_invitation(&wire, &tag, 4)?;
    let refreshed = engine.receive(update.as_bytes(), peer, 5)?;
    assert!(refreshed.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    assert!(refreshed.events.is_empty());
    assert!(engine.active_session(&id, 7).is_some());
    let ended = engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "BYE", 5)?,
        peer,
        6,
    )?;
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id: ended, generation: 7, .. }] if ended == &id)
    );
    Ok(())
}

#[test]
fn unmatched_requests_regenerate_stable_private_tags_without_transaction_state(
) -> Result<(), SipError> {
    let packet = "BYE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-unmatched;rport\r\nFrom: <sip:phone@localhost>;tag=caller\r\nTo: <sip:100@localhost>\r\nCall-ID: unmatched\r\nCSeq: 1 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n";
    let peer = "127.0.0.1:5061".parse()?;
    let mut engine = Engine::new(config()?, [7; 32]);
    let original = engine.receive(packet.as_bytes(), peer, 0)?;
    let original_tag =
        response_tag(&original.datagrams[0].bytes)?.ok_or(SipError::InvalidPacket)?;
    assert_eq!(original_tag.len(), 64);
    let repeated = engine.receive(packet.as_bytes(), peer, 1)?;
    assert_eq!(repeated.datagrams[0].bytes, original.datagrams[0].bytes);
    let changed = engine.receive(
        packet.replace("tag=caller", "tag=another").as_bytes(),
        peer,
        2,
    )?;
    assert_ne!(changed.datagrams[0].bytes, original.datagrams[0].bytes);
    assert_ne!(
        response_tag(&changed.datagrams[0].bytes)?,
        Some(original_tag)
    );
    assert!(changed.datagrams[0].bytes.starts_with(b"SIP/2.0 481"));
    let mut other = Engine::new(config()?, [8; 32]);
    assert_ne!(
        other.receive(packet.as_bytes(), peer, 0)?.datagrams[0].bytes,
        original.datagrams[0].bytes
    );
    let supplied = packet.replace(
        "To: <sip:100@localhost>",
        "To: <sip:100@localhost>;tag=supplied",
    );
    let reply = engine.receive(supplied.as_bytes(), peer, 3)?;
    let reply = match SipMessage::try_from(reply.datagrams[0].bytes.as_slice())? {
        SipMessage::Response(reply) => reply,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    assert_eq!(
        reply
            .to_header()?
            .typed()?
            .tag()
            .map(|tag| tag.value().to_owned()),
        Some("supplied".to_owned())
    );
    let mut provisional = Engine::new(config()?, [7; 32]);
    let (accepted, _, _) = authenticated_invite(&mut provisional)?;
    assert_eq!(response_tag(&accepted.datagrams[0].bytes)?, None);
    Ok(())
}

fn response_tag(packet: &[u8]) -> Result<Option<String>, SipError> {
    let reply = match SipMessage::try_from(packet)? {
        SipMessage::Response(reply) => reply,
        SipMessage::Request(_) => return Err(SipError::InvalidPacket),
    };
    Ok(reply
        .to_header()?
        .typed()?
        .tag()
        .map(|tag| tag.value().to_owned()))
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

#[test]
fn invitations_offer_telephone_events_at_each_supported_audio_clock() -> Result<(), SipError> {
    let mut settings = config()?;
    settings.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(settings.clone(), [9; 32]);
    let output = engine.originate(outbound(), 0)?;
    let invitation = request(&output.datagrams[0].bytes)?;
    let source = std::str::from_utf8(&invitation.body)?;
    assert!(source.contains("a=rtpmap:102 telephone-event/48000\r\n"));
    assert!(source.contains("a=fmtp:101 0-15\r\n"));
    assert!(source.contains("a=fmtp:102 0-15\r\n"));
    let session = Session::parse(source, &[rusterisk_config::Codec::Opus])?;
    assert_eq!(session.telephone.map(|format| format.rate()), Some(48000));
    settings.endpoints[0].codecs = [rusterisk_config::Codec::Pcmu].into_iter().collect();
    let mut engine = Engine::new(settings, [9; 32]);
    let output = engine.originate(outbound(), 0)?;
    let invitation = request(&output.datagrams[0].bytes)?;
    let source = std::str::from_utf8(&invitation.body)?;
    assert!(!source.contains("telephone-event/48000"));
    assert!(source.contains("a=fmtp:101 0-15\r\n"));
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
        timeout_ms: Some(30_000),
    }
}

#[test]
fn calls_without_a_dial_deadline_keep_ringing_after_provisional_responses() -> Result<(), SipError>
{
    for timeout in [None, Some(30_000)] {
        let mut config = config()?;
        config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
        let mut engine = Engine::new(config, [9; 32]);
        let mut call = outbound();
        call.timeout_ms = timeout;
        let invite = engine.originate(call, 0)?.datagrams.remove(0).bytes;
        engine.receive(&provisional(&invite)?, "127.0.0.1:5061".parse()?, 100)?;
        let later = engine.tick(60_000)?;
        match timeout {
            None => {
                assert!(later.events.is_empty());
                assert!(later.datagrams.is_empty());
                let ended = engine.cancel("outgoing", 7, 60_001)?;
                assert!(ended
                    .datagrams
                    .iter()
                    .any(|packet| packet.bytes.starts_with(b"CANCEL ")));
            }
            Some(_) => assert!(matches!(
                &later.events[..],
                [SipEvent::Ended { cause: 19, .. }]
            )),
        }
    }
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let mut call = outbound();
    call.timeout_ms = None;
    engine.originate(call, 0)?;
    assert!(matches!(
        &engine.tick(32_000)?.events[..],
        [SipEvent::Ended { cause: 19, .. }]
    ));
    Ok(())
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
fn an_outgoing_call_accepts_peer_direction_updates_and_closes_an_unacknowledged_update(
) -> Result<(), SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(config, [9; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let wire = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let invite = request(&wire)?;
    let answer = answered(&wire)?;
    engine.receive(&answer, peer, 1)?;
    let SipMessage::Response(answer) = SipMessage::try_from(answer.as_slice())? else {
        return Err(SipError::InvalidPacket);
    };
    for (sequence, version, direction, expected) in [
        (1, 2, "sendonly", rusterisk_sip::Direction::SendOnly),
        (2, 3, "sendrecv", rusterisk_sip::Direction::SendRecv),
    ] {
        let body = std::str::from_utf8(&answer.body)?
            .replace("o=- 1 1 IN", &format!("o=- 1 {version} IN"))
            .replace("a=sendrecv", &format!("a={direction}"));
        let offer = format!("INVITE sip:rusterisk@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-remote-update-{sequence};rport\r\nFrom: {};tag=callee\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), body.len());
        let now = u64::from(sequence) * 1000;
        let updated = engine.receive(offer.as_bytes(), peer, now)?;
        assert!(updated.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
        assert!(
            matches!(&updated.events[..], [SipEvent::PeerDirection { id, generation: 7, direction }] if id == "outgoing" && *direction == expected)
        );
        assert_eq!(
            engine
                .active_session("outgoing", 7)
                .ok_or(SipError::InvalidPacket)?
                .direction,
            expected
        );
        let repeated = engine.receive(offer.as_bytes(), peer, now + 1)?;
        assert!(repeated.events.is_empty());
        assert_eq!(repeated.datagrams[0].bytes, updated.datagrams[0].bytes);
        assert_eq!(
            engine.tick(now + 500)?.datagrams[0].bytes,
            updated.datagrams[0].bytes
        );
        if sequence == 1 {
            engine.receive(
                &incoming_request(&updated.datagrams[0].bytes, "ACK", sequence)?,
                peer,
                now + 501,
            )?;
            assert!(engine.tick(now + 999)?.datagrams.is_empty());
        }
    }
    let expired = engine.tick(34_000)?;
    assert!(matches!(
        &expired.events[..],
        [SipEvent::Ended {
            generation: 7,
            cause: 102,
            ..
        }]
    ));
    assert!(expired
        .datagrams
        .iter()
        .any(|packet| packet.bytes.starts_with(b"BYE ")));
    assert!(engine.active_session("outgoing", 7).is_none());
    assert!(engine.tick(34_001)?.events.is_empty());
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
        SipEvent::AttendedTransferRequested { .. }
        | SipEvent::ForkTeardownFailed { .. }
        | SipEvent::BlindTransferRequested { .. }
        | SipEvent::ReferSubscriptionEnded { .. }
        | SipEvent::EndpointContact { .. }
        | SipEvent::DtmfReceived { .. }
        | SipEvent::PeerDirection { .. }
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
            SipEvent::AttendedTransferRequested { .. }
            | SipEvent::ForkTeardownFailed { .. }
            | SipEvent::BlindTransferRequested { .. }
            | SipEvent::ReferSubscriptionEnded { .. }
            | SipEvent::EndpointContact { .. }
            | SipEvent::DtmfReceived { .. }
            | SipEvent::PeerDirection { .. }
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

#[test]
fn sending_info_uses_the_existing_dialog_and_retries_without_ending_the_call(
) -> Result<(), SipError> {
    for incoming in [false, true] {
        let mut config = config()?;
        config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
        let mut engine = Engine::new(config, [9; 32]);
        let peer = "127.0.0.1:5061".parse()?;
        let (id, generation) = if incoming {
            let (output, _, _) = authenticated_invite(&mut engine)?;
            let id = incoming_id(&output)?;
            let answer = engine.answer(&id, 9, "127.0.0.1:10000".parse()?, 10)?;
            engine.receive(
                &incoming_request(&answer.datagrams[0].bytes, "ACK", 2)?,
                peer,
                11,
            )?;
            (id, 9)
        } else {
            let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
            engine.receive(&answered(&invite)?, peer, 1)?;
            ("outgoing".into(), 7)
        };
        let output = engine.send_info(&id, generation, rusterisk_core::DtmfDigit::Hash, 120, 20)?;
        assert!(output.events.is_empty());
        let wire = &output.datagrams[0].bytes;
        let info = request(wire)?;
        assert_eq!(info.method, Method::Info);
        assert_eq!(info.body, b"Signal=#\r\nDuration=120\r\n");
        assert!(info.to_header()?.typed()?.tag().is_some());
        assert!(info.from_header()?.typed()?.tag().is_some());
        assert_eq!(engine.tick(520)?.datagrams[0].bytes, *wire);
        engine.receive(&transaction_response(wire, 200)?, peer, 521)?;
        assert!(engine.tick(1520)?.datagrams.is_empty());
        assert!(engine.active_session(&id, generation).is_some());
        assert!(engine
            .send_info(
                &id,
                generation + 1,
                rusterisk_core::DtmfDigit::Five,
                120,
                1521
            )
            .is_err());
    }
    Ok(())
}
#[path = "identity/forked_answers.rs"]
mod forked_answers;
#[path = "support/twins.rs"]
mod twins;
#[path = "identity/wake_twins.rs"]
mod wake_twins;
