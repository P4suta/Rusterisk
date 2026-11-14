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

#[test]
fn authenticated_identity_is_distinct_from_caller_id_and_retransmission_is_idempotent(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
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
    let cancelled = engine.cancel("outgoing", 7, 1)?;
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
