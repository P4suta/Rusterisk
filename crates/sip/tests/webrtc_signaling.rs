use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, SipMessage, Uri,
};
use rusterisk_config::Config;
use rusterisk_sip::{
    Engine, Outbound, Output, SessionSecurity, SipError, SipEvent, WebRtcDescription,
};
#[path = "support/webrtc.rs"]
mod descriptions;
#[path = "webrtc_signaling/direction.rs"]
mod direction;

fn config(webrtc: bool) -> Result<Config, SipError> {
    Ok(Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=test\n", &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[aor]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=aor\nallow=ulaw,alaw,opus\nwebrtc={}\n", if webrtc { "yes" } else { "no" }), "", "")?)
}

fn outbound() -> Result<Outbound, SipError> {
    Ok(Outbound {
        id: "browser-call".into(),
        generation: 7,
        endpoint: "phone".into(),
        caller: "100".into(),
        local_rtp: "127.0.0.1:10000".parse()?,
        timeout_ms: Some(3000),
    })
}

fn invite(body: &str, authorization: &str, sequence: u32) -> String {
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-browser-{sequence};rport\r\nFrom: <sip:spoofed@localhost>;tag=browser\r\nTo: <sip:100@localhost>\r\nCall-ID: browser-call\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

fn authenticated(engine: &mut Engine, body: &str) -> Result<Output, SipError> {
    let peer = "127.0.0.1:5061".parse()?;
    let challenged = engine.receive(invite(body, "", 1).as_bytes(), peer, 0)?;
    let response = SipMessage::try_from(std::str::from_utf8(
        &challenged
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?)?;
    let SipMessage::Response(response) = response else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 401);
    let challenge = WwwAuthenticate::parse(
        response
            .www_authenticate_header()
            .ok_or(SipError::InvalidPacket)?
            .value(),
    )?;
    let ack = format!("ACK sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-browser-1;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: browser-call\r\nCSeq: 1 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", response.from_header()?.value(), response.to_header()?.value());
    engine.receive(ack.as_bytes(), peer, 0)?;
    let uri = Uri::try_from("sip:100@localhost")?;
    let qop = AuthQop::Auth {
        cnonce: "browser-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "test-password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"browser-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    engine.receive(invite(body, &authorization, 2).as_bytes(), peer, 1)
}

#[test]
fn outgoing_browser_calls_send_the_owned_offer_and_refuse_profile_downgrades(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    assert!(matches!(
        engine.originate(outbound()?, 0),
        Err(SipError::Unsupported)
    ));
    let description = WebRtcDescription::parse(&descriptions::offer())?;
    let output = engine.originate_with_sdp(outbound()?, &description, 1)?;
    let message = SipMessage::try_from(std::str::from_utf8(
        &output
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?)?;
    let SipMessage::Request(request) = message else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(request.body, description.as_str().as_bytes());
    let mut plaintext = Engine::new(config(false)?, [9; 32]);
    assert!(matches!(
        plaintext.originate_with_sdp(outbound()?, &description, 0),
        Err(SipError::Unsupported)
    ));
    Ok(())
}

#[test]
fn resolved_origination_keeps_the_configured_uri_and_uses_the_physical_peer() -> Result<(), SipError>
{
    let mut config = config(false)?;
    config.aors[0].contact = Some("sip:phone@peer.invalid:5070;transport=udp".into());
    let mut engine = Engine::new(config, [9; 32]);
    let peer = "127.0.0.2:40999".parse()?;
    let output = engine.originate_to(outbound()?, peer, 0)?;
    let packet = output.datagrams.first().ok_or(SipError::InvalidPacket)?;
    assert_eq!(packet.peer, peer);
    let SipMessage::Request(request) = SipMessage::try_from(std::str::from_utf8(&packet.bytes)?)?
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(
        request.uri,
        Uri::try_from("sip:phone@peer.invalid:5070;transport=udp")?
    );
    assert!(std::str::from_utf8(&request.body)?.contains("RTP/AVP"));
    Ok(())
}

#[test]
fn unacceptable_successful_answers_are_acked_and_ended_without_an_answered_event(
) -> Result<(), SipError> {
    let plain = descriptions::offer().replace("UDP/TLS/RTP/SAVPF", "RTP/AVP");
    let browser_answer = descriptions::offer().replace("a=setup:actpass", "a=setup:passive");
    for (webrtc, body) in [
        (true, String::new()),
        (true, plain),
        (false, browser_answer),
        (true, descriptions::offer()),
    ] {
        let mut engine = Engine::new(config(webrtc)?, [7; 32]);
        let offer = if webrtc {
            engine.originate_with_sdp(
                outbound()?,
                &WebRtcDescription::parse(&descriptions::offer())?,
                0,
            )?
        } else {
            engine.originate(outbound()?, 0)?
        };
        let packet = offer.datagrams.first().ok_or(SipError::InvalidPacket)?;
        let SipMessage::Request(request) =
            SipMessage::try_from(std::str::from_utf8(&packet.bytes)?)?
        else {
            return Err(SipError::InvalidPacket);
        };
        let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=peer\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@127.0.0.1:5061>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header()?.value(), request.from_header()?.value(), request.to_header()?.value(), request.call_id_header()?.value(), request.cseq_header()?.value(), body.len());
        let result = engine.receive(response.as_bytes(), packet.peer, 1)?;
        let methods: Result<Vec<_>, SipError> = result
            .datagrams
            .iter()
            .map(|packet| {
                assert_eq!(packet.peer, "127.0.0.1:5061".parse()?);
                match SipMessage::try_from(std::str::from_utf8(&packet.bytes)?)? {
                    SipMessage::Request(request) => Ok(request.method),
                    SipMessage::Response(_) => Err(SipError::InvalidPacket),
                }
            })
            .collect();
        assert_eq!(methods?, [Method::Ack, Method::Bye]);
        assert_eq!(result.events.len(), 1);
        assert!(
            matches!(result.events.first(), Some(SipEvent::Ended { id, generation: 7, cause: 21 }) if id == "browser-call")
        );
        let duplicate = engine.receive(response.as_bytes(), packet.peer, 2)?;
        assert_eq!(duplicate.datagrams.len(), 1);
        assert!(duplicate.events.is_empty());
        let SipMessage::Request(ack) =
            SipMessage::try_from(std::str::from_utf8(&duplicate.datagrams[0].bytes)?)?
        else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(ack.method, Method::Ack);
    }
    Ok(())
}

#[test]
fn incoming_browser_answers_keep_the_checked_ice_and_dtls_description() -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let incoming = authenticated(&mut engine, &descriptions::offer())?;
    let id = incoming
        .events
        .iter()
        .find_map(|event| match event {
            SipEvent::Incoming { id, session, .. }
                if matches!(session.security, SessionSecurity::WebRtc(_)) =>
            {
                Some(id.clone())
            }
            SipEvent::AttendedTransferRequested { .. }
            | SipEvent::BlindTransferRequested { .. }
            | SipEvent::ReferSubscriptionEnded { .. }
            | SipEvent::EndpointContact { .. }
            | SipEvent::PeerDirection { .. }
            | SipEvent::DtmfReceived { .. }
            | SipEvent::Ringing { .. }
            | SipEvent::Answered { .. }
            | SipEvent::Ended { .. }
            | SipEvent::Incoming { .. }
            | SipEvent::IncomingEnded { .. } => None,
        })
        .ok_or(SipError::InvalidPacket)?;
    assert!(matches!(
        engine.answer(&id, 7, "127.0.0.1:10000".parse()?, 2),
        Err(SipError::Unsupported)
    ));
    assert!(engine.pending(&id).is_some());
    let local = WebRtcDescription::parse(
        &descriptions::offer().replace("a=setup:actpass", "a=setup:active"),
    )?;
    let answered = engine.answer_with_sdp(&id, 7, "127.0.0.1:10000".parse()?, &local, 3)?;
    let message = SipMessage::try_from(std::str::from_utf8(
        &answered
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?)?;
    let SipMessage::Response(response) = message else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 200);
    assert_eq!(response.body, local.as_str().as_bytes());
    assert!(answered.events.iter().any(|event| matches!(event, SipEvent::Answered { session, .. } if matches!(session.security, SessionSecurity::WebRtc(_)))));
    Ok(())
}

#[test]
fn authenticated_invites_must_match_the_endpoint_media_profile() -> Result<(), SipError> {
    for (webrtc, body) in [
        (
            true,
            "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 10000 RTP/AVP 0\r\n".to_owned(),
        ),
        (false, descriptions::offer()),
    ] {
        let mut engine = Engine::new(config(webrtc)?, [7; 32]);
        let output = authenticated(&mut engine, &body)?;
        assert!(output.events.is_empty());
        assert!(output
            .datagrams
            .first()
            .is_some_and(|packet| packet.bytes.starts_with(b"SIP/2.0 488 ")));
    }
    Ok(())
}
