use super::{
    authenticated_invite, authenticated_invite_with_headers, config, incoming_id, incoming_request,
    reauthenticate,
};
use rsipstack::sip::SipMessage;
use rusterisk_sip::{Engine, Output, SipError, SipEvent};

fn unsupported(output: &Output, expected: &str) -> Result<(), SipError> {
    assert_eq!(output.datagrams.len(), 1);
    let SipMessage::Response(response) =
        SipMessage::try_from(output.datagrams[0].bytes.as_slice())?
    else {
        return Err(SipError::InvalidPacket);
    };
    assert_eq!(response.status_code.code(), 420);
    assert!(output.events.is_empty());
    let values: Vec<_> = response
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("Unsupported"))
        .map(|header| header.value())
        .collect();
    assert_eq!(values, [expected]);
    Ok(())
}

#[test]
fn required_replaces_is_rejected_before_initial_call_admission() -> Result<(), SipError> {
    for (headers, expected) in [
        ("Require: replaces\r\n", "replaces"),
        ("rEqUiRe: \tRePlAcEs \t\r\n", "replaces"),
        ("Require: 100rel, replaces\r\n", "100rel, replaces"),
        (
            "Require: 100rel\r\nRequire: replaces\r\n",
            "100rel, replaces",
        ),
    ] {
        let mut engine = Engine::new(config()?, [7; 32]);
        let (rejected, _, _) = authenticated_invite_with_headers(&mut engine, headers)?;
        unsupported(&rejected, expected)?;
    }
    Ok(())
}

#[test]
fn bare_replaces_is_an_ignored_extension_without_a_required_option_tag() -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let (incoming, _, _) = authenticated_invite_with_headers(
        &mut engine,
        "Replaces: unrelated;to-tag=target;from-tag=source\r\nSupported: replaces\r\n",
    )?;
    let id = incoming_id(&incoming)?;
    assert!(engine.pending(&id).is_some());
    assert!(incoming.datagrams[0].bytes.starts_with(b"SIP/2.0 100"));
    Ok(())
}

#[test]
fn rejected_required_options_replay_exactly_and_stop_retrying_after_the_error_ack(
) -> Result<(), SipError> {
    let mut engine = Engine::new(config()?, [7; 32]);
    let peer = "127.0.0.1:5061".parse()?;
    let (rejected, wire, _) =
        authenticated_invite_with_headers(&mut engine, "Require: replaces\r\n")?;
    unsupported(&rejected, "replaces")?;
    let repeated = wire.replace(";branch=z9hG4bK-2;rport", ";rport;branch=z9hG4bK-2");
    for packet in [wire.as_str(), repeated.as_str()] {
        let replayed = engine.receive(packet.as_bytes(), peer, 2)?;
        assert!(replayed.events.is_empty());
        assert_eq!(replayed.datagrams[0].bytes, rejected.datagrams[0].bytes);
    }
    assert_eq!(
        engine.tick(501)?.datagrams[0].bytes,
        rejected.datagrams[0].bytes
    );
    let ack = incoming_request(&rejected.datagrams[0].bytes, "ACK", 2)?;
    let ack = std::str::from_utf8(&ack)?
        .replace("z9hG4bK-ACK-2", "z9hG4bK-2")
        .replace(
            "Max-Forwards: 70\r\n",
            "Max-Forwards: 70\r\nRequire: replaces\r\n",
        );
    let acknowledged = engine.receive(ack.as_bytes(), peer, 502)?;
    assert!(acknowledged.datagrams.is_empty());
    assert!(acknowledged.events.is_empty());
    assert!(engine.tick(1500)?.datagrams.is_empty());
    let replayed = engine.receive(repeated.as_bytes(), peer, 1501)?;
    assert_eq!(replayed.datagrams[0].bytes, rejected.datagrams[0].bytes);
    assert!(replayed.events.is_empty());
    Ok(())
}

#[test]
fn a_required_option_rejection_preserves_another_established_incoming_dialog(
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
    let original = engine
        .active_session(&id, 7)
        .ok_or(SipError::InvalidPacket)?
        .clone();
    let another = wire
        .replace("tag=caller", "tag=required")
        .replace("z9hG4bK-2", "z9hG4bK-required")
        .replacen("Content-Type: ", "Require: replaces\r\nContent-Type: ", 1);
    let another = reauthenticate(&another, 2)?;
    let rejected = engine.receive(another.as_bytes(), peer, 5)?;
    unsupported(&rejected, "replaces")?;
    assert_eq!(engine.active_session(&id, 7), Some(&original));
    let ended = engine.receive(
        &incoming_request(&answered.datagrams[0].bytes, "BYE", 3)?,
        peer,
        6,
    )?;
    assert!(
        matches!(&ended.events[..], [SipEvent::Ended { id: ended_id, generation: 7, .. }] if ended_id == &id)
    );
    Ok(())
}

#[test]
fn malformed_required_options_are_not_hidden_after_a_valid_tag() -> Result<(), SipError> {
    for headers in [
        "Require: replaces,\r\n",
        "Require: replaces\r\nRequire: invalid value\r\n",
        "Require: replaces;parameter\r\n",
    ] {
        let mut engine = Engine::new(config()?, [7; 32]);
        let peer = "127.0.0.1:5061".parse()?;
        let (rejected, wire, _) = authenticated_invite_with_headers(&mut engine, headers)?;
        assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 400"));
        assert!(rejected.events.is_empty());
        let repeated = engine.receive(wire.as_bytes(), peer, 2)?;
        assert_eq!(repeated.datagrams[0].bytes, rejected.datagrams[0].bytes);
        assert!(repeated.events.is_empty());
        assert_eq!(
            engine.tick(501)?.datagrams[0].bytes,
            rejected.datagrams[0].bytes
        );
        let ack = incoming_request(&rejected.datagrams[0].bytes, "ACK", 2)?;
        let ack = std::str::from_utf8(&ack)?.replace("z9hG4bK-ACK-2", "z9hG4bK-2");
        assert!(engine
            .receive(ack.as_bytes(), peer, 502)?
            .datagrams
            .is_empty());
        assert!(engine.tick(1500)?.datagrams.is_empty());
    }
    Ok(())
}
