use super::{authenticated, config, descriptions, invite, outbound};
use rsipstack::sip::{HeadersExt, Request, Response, SipMessage};
use rusterisk_sip::{Direction, Engine, Output, Session, SipError, SipEvent, WebRtcDescription};

fn local_offer() -> String {
    descriptions::offer()
        .replace("o=- 1 1 IN", "o=- 20 9 IN")
        .replace("a=ice-ufrag:peer", "a=ice-ufrag:local")
        .replace(
            "PrivateIcePasswordForPeer22",
            "PrivateIcePasswordForLocal22",
        )
        .replace(&"00:".repeat(31), &"11:".repeat(31))
        + "a=tls-id:LocalEstablishedDtlsId22\r\n"
}

fn peer_offer() -> String {
    descriptions::offer() + "a=tls-id:RemoteEstablishedDtlsId2\r\n"
}

fn response(output: &Output) -> Result<Response, SipError> {
    let bytes = &output
        .datagrams
        .first()
        .ok_or(SipError::InvalidPacket)?
        .bytes;
    match SipMessage::try_from(std::str::from_utf8(bytes)?)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(SipError::InvalidPacket),
    }
}

fn assert_answer(
    output: &Output,
    direction: Direction,
    version: u64,
) -> Result<Response, SipError> {
    let response = response(output)?;
    assert_eq!(response.status_code.code(), 200);
    let body = std::str::from_utf8(&response.body)?;
    assert!(body.contains(&format!("o=- 20 {version} IN")));
    assert!(body.contains("a=ice-ufrag:local\r\n"));
    assert!(body.contains("a=ice-pwd:PrivateIcePasswordForLocal22\r\n"));
    assert!(body.contains(&format!("a=fingerprint:sha-256 {}00", "11:".repeat(31))));
    assert!(body.contains("a=tls-id:LocalEstablishedDtlsId22\r\n"));
    assert!(body.contains("a=setup:active\r\n"));
    let session = Session::parse(body, &[rusterisk_config::Codec::Pcmu])?;
    assert_eq!(session.direction, direction);
    Ok(response)
}

fn acknowledge(engine: &mut Engine, response: &Response, sequence: u32) -> Result<(), SipError> {
    let contact =
        rsipstack::sip::headers::typed::Contact::parse(response.contact_header()?.value())?;
    let ack = format!("ACK {} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-direction-ack-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", contact.uri, response.from_header()?.value(), response.to_header()?.value(), response.call_id_header()?.value());
    engine.receive(ack.as_bytes(), "127.0.0.1:5061".parse()?, 5)?;
    Ok(())
}

#[test]
fn incoming_direction_answers_keep_local_security_and_established_roles() -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let incoming = authenticated(&mut engine, &peer_offer())?;
    let id = incoming
        .events
        .iter()
        .find_map(|event| match event {
            SipEvent::Incoming { id, .. } => Some(id.clone()),
            SipEvent::EndpointContact { .. }
            | SipEvent::PeerDirection { .. }
            | SipEvent::DtmfReceived { .. }
            | SipEvent::Ringing { .. }
            | SipEvent::Answered { .. }
            | SipEvent::Ended { .. }
            | SipEvent::IncomingEnded { .. } => None,
        })
        .ok_or(SipError::InvalidPacket)?;
    let local =
        WebRtcDescription::parse(&local_offer().replace("a=setup:actpass", "a=setup:active"))?;
    let initial = engine.answer_with_sdp(&id, 7, "127.0.0.1:10000".parse()?, &local, 2)?;
    let initial = response(&initial)?;
    acknowledge(&mut engine, &initial, 2)?;
    for (sequence, direction, expected, peer_direction, setup) in [
        (
            3,
            "sendonly",
            Direction::RecvOnly,
            Direction::SendOnly,
            "actpass",
        ),
        (
            4,
            "recvonly",
            Direction::SendOnly,
            Direction::RecvOnly,
            "passive",
        ),
        (
            5,
            "inactive",
            Direction::Inactive,
            Direction::Inactive,
            "actpass",
        ),
        (
            6,
            "sendrecv",
            Direction::SendRecv,
            Direction::SendRecv,
            "passive",
        ),
    ] {
        let body = peer_offer()
            .replace("o=- 1 1 IN", &format!("o=- 1 {} IN", sequence - 1))
            .replace("a=sendrecv", &format!("a={direction}"))
            .replace("a=setup:actpass", &format!("a=setup:{setup}"));
        let request = invite(&body, "", sequence).replace(
            "To: <sip:100@localhost>",
            &format!("To: {}", initial.to_header()?.value()),
        );
        let output = engine.receive(
            request.as_bytes(),
            "127.0.0.1:5061".parse()?,
            u64::from(sequence),
        )?;
        let answered = assert_answer(&output, expected, u64::from(sequence + 7))?;
        assert!(
            matches!(output.events.as_slice(), [SipEvent::PeerDirection { id: actual, generation: 7, direction }] if actual == &id && *direction == peer_direction)
        );
        acknowledge(&mut engine, &answered, sequence)?;
    }
    Ok(())
}

fn outgoing_dialog() -> Result<(Engine, Request, String), SipError> {
    let body = peer_offer().replace("a=setup:actpass", "a=setup:passive");
    let (engine, request) = outgoing_with_answer(&body)?;
    Ok((engine, request, body))
}

fn outgoing_with_answer(body: &str) -> Result<(Engine, Request), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let description = WebRtcDescription::parse(&local_offer())?;
    let output = engine.originate_with_sdp(outbound()?, &description, 0)?;
    let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    let SipMessage::Request(request) = SipMessage::try_from(std::str::from_utf8(&packet.bytes)?)?
    else {
        return Err(SipError::InvalidPacket);
    };
    let answer = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=peer\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value(), body.len());
    let answered = engine.receive(answer.as_bytes(), packet.peer, 1)?;
    assert!(matches!(
        answered.events.as_slice(),
        [SipEvent::Answered { .. }]
    ));
    Ok((engine, request))
}

fn reoffer(invitation: &Request, body: &str, sequence: u32) -> Result<String, SipError> {
    let local =
        rsipstack::sip::headers::typed::Contact::parse(invitation.contact_header()?.value())?;
    Ok(format!("INVITE {} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-direction-{sequence};rport\r\nFrom: {};tag=peer\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", local.uri, invitation.to_header()?.value(), invitation.from_header()?.value(), invitation.call_id_header()?.value(), body.len()))
}

#[test]
fn outgoing_direction_answers_keep_local_security_and_established_roles() -> Result<(), SipError> {
    let (mut engine, invitation, _) = outgoing_dialog()?;
    for (sequence, direction, expected, peer_direction) in [
        (1, "sendonly", Direction::RecvOnly, Direction::SendOnly),
        (2, "recvonly", Direction::SendOnly, Direction::RecvOnly),
        (3, "inactive", Direction::Inactive, Direction::Inactive),
        (4, "sendrecv", Direction::SendRecv, Direction::SendRecv),
    ] {
        let body = peer_offer()
            .replace("o=- 1 1 IN", &format!("o=- 1 {} IN", sequence + 1))
            .replace("a=sendrecv", &format!("a={direction}"));
        let request = reoffer(&invitation, &body, sequence)?;
        let output = engine.receive(
            request.as_bytes(),
            "127.0.0.1:5061".parse()?,
            u64::from(sequence + 1),
        )?;
        let answered = assert_answer(&output, expected, u64::from(sequence + 9))?;
        assert!(
            matches!(output.events.as_slice(), [SipEvent::PeerDirection { id, generation: 7, direction }] if id == "browser-call" && *direction == peer_direction)
        );
        acknowledge(&mut engine, &answered, sequence)?;
    }
    Ok(())
}

#[test]
fn direction_updates_refuse_security_or_media_replacement_without_changing_the_session(
) -> Result<(), SipError> {
    for (previous, replacement) in [
        ("a=ice-ufrag:peer", "a=ice-ufrag:other"),
        (
            "PrivateIcePasswordForPeer22",
            "ReplacementIcePasswordForPeer22",
        ),
        ("RemoteEstablishedDtlsId2", "ReplacementEstablishedId22"),
        ("a=fingerprint:sha-256 00:", "a=fingerprint:sha-256 22:"),
        ("a=setup:actpass", "a=setup:active"),
        ("m=audio 9", "m=audio 10002"),
        ("a=rtpmap:0 PCMU/8000", "a=rtpmap:0 PCMA/8000"),
    ] {
        let (mut engine, invitation, _) = outgoing_dialog()?;
        let body = peer_offer()
            .replace("o=- 1 1 IN", "o=- 1 2 IN")
            .replace("a=sendrecv", "a=inactive")
            .replace(previous, replacement);
        let request = reoffer(&invitation, &body, 1)?;
        let output = engine.receive(request.as_bytes(), "127.0.0.1:5061".parse()?, 2)?;
        assert_eq!(response(&output)?.status_code.code(), 488);
        assert!(output.events.is_empty());
        let session = engine
            .active_session("browser-call", 7)
            .ok_or(SipError::InvalidPacket)?;
        assert_eq!(session.direction, Direction::SendRecv);
    }
    Ok(())
}

#[test]
fn legacy_direction_answers_omit_unnegotiated_tls_identifiers() -> Result<(), SipError> {
    let answer = descriptions::offer().replace("a=setup:actpass", "a=setup:passive");
    let (mut engine, invitation) = outgoing_with_answer(&answer)?;
    let body = answer
        .replace("o=- 1 1 IN", "o=- 1 2 IN")
        .replace("a=sendrecv", "a=inactive");
    let output = engine.receive(
        reoffer(&invitation, &body, 1)?.as_bytes(),
        "127.0.0.1:5061".parse()?,
        2,
    )?;
    let response = response(&output)?;
    assert_eq!(response.status_code.code(), 200);
    let body = std::str::from_utf8(&response.body)?;
    assert!(!body.lines().any(|line| line.starts_with("a=tls-id:")));
    assert!(body.contains("a=setup:active\r\n"));
    assert!(body.contains("a=ice-ufrag:local\r\n"));
    Ok(())
}

#[test]
fn initial_legacy_answers_omit_unnegotiated_tls_identifiers() -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let incoming = authenticated(&mut engine, &descriptions::offer())?;
    let id = incoming
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
    let description =
        WebRtcDescription::parse(&local_offer().replace("a=setup:actpass", "a=setup:active"))?;
    let output = engine.answer_with_sdp(&id, 7, "127.0.0.1:10000".parse()?, &description, 2)?;
    let response = response(&output)?;
    assert_eq!(response.status_code.code(), 200);
    let body = std::str::from_utf8(&response.body)?;
    assert!(!body.lines().any(|line| line.starts_with("a=tls-id:")));
    assert!(body.contains("a=ice-ufrag:local\r\n"));
    assert!(body.contains("a=setup:active\r\n"));
    Ok(())
}
