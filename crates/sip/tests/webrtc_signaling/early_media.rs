use super::{authenticated, config, descriptions};
use rsipstack::sip::{HeadersExt, Response, SipMessage};
use rusterisk_sip::{Engine, Output, SipError, SipEvent, WebRtcDescription};
use std::net::SocketAddr;

fn local_description() -> String {
    descriptions::offer()
        .replace("o=- 1 1 IN", "o=- 20 9 IN")
        .replace("a=ice-ufrag:peer", "a=ice-ufrag:local")
        .replace(
            "PrivateIcePasswordForPeer22",
            "PrivateIcePasswordForLocal22",
        )
        .replace(&"00:".repeat(31), &"11:".repeat(31))
        .replace("a=setup:actpass", "a=setup:active")
}

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

#[test]
fn browser_progress_and_answer_share_the_effective_local_security_description(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let incoming = authenticated(&mut engine, &descriptions::offer())?;
    let [SipEvent::Incoming { id, .. }] = &incoming.events[..] else {
        return Err(SipError::InvalidPacket);
    };
    let local: SocketAddr = "127.0.0.1:10000".parse()?;
    let local_sdp = local_description();
    let description = WebRtcDescription::parse(&format!(
        "{local_sdp}a=tls-id:UnnegotiatedLocalDtlsId22\r\n"
    ))?;
    let progress = engine.progress_with_sdp(id, 9, local, &description, 2)?;
    let provisional = response(&progress, 183)?;
    assert!(progress.events.is_empty());
    assert_eq!(provisional.body, local_sdp.as_bytes());
    assert!(engine.pending(id).is_some());
    assert!(engine.active_session(id, 9).is_none());
    let answer = engine.answer_with_sdp(id, 9, local, &description, 3)?;
    let final_response = response(&answer, 200)?;
    assert_eq!(final_response.body, provisional.body);
    assert_eq!(final_response.to_header()?, provisional.to_header()?);
    assert_eq!(
        final_response.contact_header()?,
        provisional.contact_header()?
    );
    assert!(matches!(
        &answer.events[..],
        [SipEvent::Answered { id: actual, generation: 9, .. }] if actual == id
    ));
    assert!(engine.pending(id).is_none());
    assert!(engine.active_session(id, 9).is_some());
    Ok(())
}

#[test]
fn changed_progress_identity_or_security_cannot_replace_the_published_answer(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config(true)?, [7; 32]);
    let incoming = authenticated(&mut engine, &descriptions::offer())?;
    let [SipEvent::Incoming { id, .. }] = &incoming.events[..] else {
        return Err(SipError::InvalidPacket);
    };
    let local: SocketAddr = "127.0.0.1:10000".parse()?;
    let local_sdp = local_description();
    let description = WebRtcDescription::parse(&local_sdp)?;
    let wire = engine
        .pending(id)
        .ok_or(SipError::InvalidPacket)?
        .request
        .to_string();
    let progress = engine.progress_with_sdp(id, 9, local, &description, 2)?;
    for (generation, address) in [(10, local), (9, "127.0.0.1:10004".parse()?)] {
        assert!(engine
            .progress_with_sdp(id, generation, address, &description, 3)
            .is_err());
        assert!(engine
            .answer_with_sdp(id, generation, address, &description, 3)
            .is_err());
    }
    for changed in [
        local_sdp.replace("a=ice-ufrag:local", "a=ice-ufrag:replacement"),
        local_sdp.replace("PrivateIcePasswordForLocal22", "ReplacementIcePassword22"),
        local_sdp.replace(&"11:".repeat(31), &"22:".repeat(31)),
        local_sdp.replace("a=setup:active", "a=setup:passive"),
    ] {
        let changed = WebRtcDescription::parse(&changed)?;
        assert!(engine.progress_with_sdp(id, 9, local, &changed, 3).is_err());
        assert!(engine.answer_with_sdp(id, 9, local, &changed, 3).is_err());
    }
    assert!(engine.pending(id).is_some());
    assert!(engine.active_session(id, 9).is_none());
    let replayed = engine.receive(wire.as_bytes(), "127.0.0.1:5061".parse()?, 4)?;
    assert_eq!(replayed.datagrams[0].bytes, progress.datagrams[0].bytes);
    assert!(replayed.events.is_empty());
    let answer = engine.answer_with_sdp(id, 9, local, &description, 5)?;
    assert_eq!(response(&answer, 200)?.body, response(&progress, 183)?.body);
    Ok(())
}
