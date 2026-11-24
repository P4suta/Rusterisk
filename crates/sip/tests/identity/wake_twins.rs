use super::{
    answered, authenticated_invite, challenge_invite, config, incoming_id, outbound, provisional,
    request,
};
use crate::twins::Twins;
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_core::Wake;
use rusterisk_sip::{Engine, RejectReason, Retained, SipError, SipEvent};

fn calling() -> Result<Engine, SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    Ok(Engine::new(config, [9; 32]))
}

fn serving() -> Result<Engine, SipError> {
    Ok(Engine::new(config()?, [7; 32]))
}

fn invited(twins: &mut Twins) -> Result<String, SipError> {
    let (incoming, _, _) = authenticated_invite(&mut serving()?)?;
    twins.apply(|engine, _| authenticated_invite(engine).map(|(output, _, _)| output))?;
    twins.advance(5)?;
    incoming_id(&incoming)
}

#[test]
fn an_answered_call_and_its_hangup_change_only_at_their_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(calling)?;
    let invite = twins
        .apply(|engine, now| engine.originate(outbound(), now))?
        .datagrams
        .remove(0)
        .bytes;
    twins.advance(600)?;
    let answer = answered(&invite)?;
    let acknowledged =
        twins.apply(|engine, now| engine.receive(&answer, "127.0.0.1:5061".parse()?, now))?;
    assert!(acknowledged
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Answered { .. })));
    twins.advance(5_000)?;
    let bye = twins.apply(|engine, now| engine.cancel("outgoing", 7, now))?;
    assert!(bye.datagrams[0].bytes.starts_with(b"BYE "));
    let output = twins.advance(80_000)?;
    assert!(output.datagrams.len() >= 6);
    assert_eq!(twins.retained(), Retained::default());
    Ok(())
}

#[test]
fn an_incoming_call_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(serving)?;
    let id = invited(&mut twins)?;
    twins.apply(|engine, now| engine.ring_at(&id, now))?;
    let waited = twins.advance(61_000)?;
    assert!(waited
        .datagrams
        .iter()
        .any(|datagram| datagram.bytes.starts_with(b"SIP/2.0 180")));
    twins.apply(|engine, now| engine.answer(&id, 9, "127.0.0.1:10000".parse()?, now))?;
    let output = twins.advance(170_000)?;
    assert!(output
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Ended { cause: 102, .. })));
    assert!(output
        .datagrams
        .iter()
        .any(|datagram| datagram.bytes.starts_with(b"BYE ")));
    assert_eq!(twins.retained().incoming, 0);
    assert_eq!(twins.retained().transactions, 0);
    Ok(())
}

#[test]
fn a_rejected_invitation_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(serving)?;
    let id = invited(&mut twins)?;
    let rejected = twins.apply(|engine, now| engine.reject(&id, RejectReason::Busy, now))?;
    assert!(rejected.datagrams[0].bytes.starts_with(b"SIP/2.0 486"));
    let output = twins.advance(40_000)?;
    assert!(output.datagrams.len() >= 3);
    assert_eq!(twins.retained(), Retained::default());
    Ok(())
}

#[test]
fn an_unanswered_call_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(calling)?;
    let invite = twins.apply(|engine, now| engine.originate(outbound(), now))?;
    assert!(invite.datagrams[0].bytes.starts_with(b"INVITE "));
    let output = twins.advance(70_000)?;
    assert!(output
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Ended { cause: 19, .. })));
    assert!(output.datagrams.len() >= 6);
    assert_eq!(twins.retained(), rusterisk_sip::Retained::default());
    assert!(twins.skipped() > 69_900);
    Ok(())
}

#[test]
fn a_ringing_call_that_its_owner_cancels_changes_only_at_its_reported_wakes() -> Result<(), SipError>
{
    let mut twins = Twins::new(calling)?;
    let invite = twins
        .apply(|engine, now| engine.originate(outbound(), now))?
        .datagrams
        .remove(0)
        .bytes;
    twins.advance(700)?;
    let ringing = provisional(&invite)?;
    twins.apply(|engine, now| engine.receive(&ringing, "127.0.0.1:5061".parse()?, now))?;
    let waited = twins.advance(600_000)?;
    assert!(waited.datagrams.is_empty());
    assert!(waited.events.is_empty());
    assert!(twins.skipped() > 599_000);
    let cancelled = twins.apply(|engine, now| engine.cancel("outgoing", 7, now))?;
    assert!(cancelled.datagrams[0].bytes.starts_with(b"CANCEL "));
    let late = answered(&invite)?;
    twins.apply(|engine, now| engine.receive(&late, "127.0.0.1:5061".parse()?, now))?;
    twins.advance(710_000)?;
    assert_eq!(twins.retained(), rusterisk_sip::Retained::default());
    Ok(())
}

#[test]
fn an_engine_driven_by_its_wakes_passes_each_instant_in_one_turn() -> Result<(), SipError> {
    let mut engine = calling()?;
    engine.originate(outbound(), 0)?;
    let mut now = 0;
    let mut instants = 0;
    while let Wake::At(at) = engine.wake() {
        now = now.max(at);
        engine.tick(now)?;
        assert!(
            !matches!(engine.wake(), Wake::At(again) if again <= now),
            "the wake stayed at {now} ms after its turn"
        );
        instants += 1;
        assert!(instants < 64, "the engine never ran out of wakes");
    }
    assert_eq!(engine.wake(), Wake::Idle);
    assert_eq!(engine.retained(), Retained::default());
    assert_eq!(now, 64_000);
    Ok(())
}

fn authenticating() -> Result<Engine, SipError> {
    let mut config = config()?;
    config.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    config.endpoints[0].outbound_auth.push("a".into());
    Ok(Engine::new(config, [9; 32]))
}

fn dialed(twins: &mut Twins) -> Result<Vec<u8>, SipError> {
    let mut invited = twins.apply(|engine, now| engine.originate(outbound(), now))?;
    Ok(invited.datagrams.remove(0).bytes)
}

fn ended(output: &rusterisk_sip::Output, expected: u16) -> bool {
    output
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Ended { cause, .. } if *cause == expected))
}

#[test]
fn a_call_cancelled_before_any_response_changes_only_at_its_reported_wakes() -> Result<(), SipError>
{
    let mut twins = Twins::new(calling)?;
    dialed(&mut twins)?;
    twins.advance(100)?;
    let cancelled = twins.apply(|engine, now| engine.cancel("outgoing", 7, now))?;
    assert!(cancelled.datagrams.is_empty());
    let output = twins.advance(70_000)?;
    assert!(output
        .datagrams
        .iter()
        .all(|datagram| datagram.bytes.starts_with(b"INVITE ")));
    assert!(output.datagrams.len() >= 5);
    assert_eq!(twins.retained(), Retained::default());
    assert!(twins.skipped() > 69_900);
    Ok(())
}

#[test]
fn a_challenged_call_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(authenticating)?;
    let invite = dialed(&mut twins)?;
    twins.advance(300)?;
    let challenge = challenge_invite(&invite, false)?;
    let challenged =
        twins.apply(|engine, now| engine.receive(&challenge, "127.0.0.1:5061".parse()?, now))?;
    assert_eq!(challenged.datagrams.len(), 2);
    assert_eq!(twins.retained().challenges, 1);
    let output = twins.advance(100_000)?;
    assert!(ended(&output, 19));
    assert_eq!(twins.retained(), Retained::default());
    assert!(twins.skipped() > 99_900);
    Ok(())
}

#[test]
fn a_forked_answer_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(calling)?;
    let invite = dialed(&mut twins)?;
    twins.advance(100)?;
    let answer = answered(&invite)?;
    twins.apply(|engine, now| engine.receive(&answer, "127.0.0.1:5061".parse()?, now))?;
    twins.advance(200)?;
    let other = std::str::from_utf8(&answer)?
        .replace("tag=callee", "tag=other")
        .into_bytes();
    let cleanup =
        twins.apply(|engine, now| engine.receive(&other, "127.0.0.1:5061".parse()?, now))?;
    assert_eq!(cleanup.datagrams.len(), 2);
    assert_eq!(twins.retained().forks, 1);
    twins.advance(5_000)?;
    let bye = twins.apply(|engine, now| engine.cancel("outgoing", 7, now))?;
    assert!(bye.datagrams[0].bytes.starts_with(b"BYE "));
    twins.advance(120_000)?;
    assert_eq!(twins.retained(), Retained::default());
    assert!(twins.skipped() > 119_800);
    Ok(())
}

#[test]
fn an_unacknowledged_direction_update_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(calling)?;
    let wire = dialed(&mut twins)?;
    let invite = request(&wire)?;
    twins.advance(100)?;
    let answer = answered(&wire)?;
    twins.apply(|engine, now| engine.receive(&answer, "127.0.0.1:5061".parse()?, now))?;
    twins.advance(1_000)?;
    let SipMessage::Response(answer) = SipMessage::try_from(answer.as_slice())? else {
        return Err(SipError::InvalidPacket);
    };
    let body = std::str::from_utf8(&answer.body)?
        .replace("o=- 1 1 IN", "o=- 1 2 IN")
        .replace("a=sendrecv", "a=sendonly");
    let offer = format!("INVITE sip:rusterisk@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-remote-update-1;rport\r\nFrom: {};tag=callee\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 INVITE\r\nContact: <sip:phone@127.0.0.1:5061>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", invite.to_header()?.value(), invite.from_header()?.value(), invite.call_id_header()?.value(), body.len());
    let updated = twins
        .apply(|engine, now| engine.receive(offer.as_bytes(), "127.0.0.1:5061".parse()?, now))?;
    assert!(updated.datagrams[0].bytes.starts_with(b"SIP/2.0 200"));
    let output = twins.advance(120_000)?;
    assert!(ended(&output, 102));
    assert!(output
        .datagrams
        .iter()
        .any(|datagram| datagram.bytes.starts_with(b"BYE ")));
    assert_eq!(twins.retained(), Retained::default());
    assert!(twins.skipped() > 118_900);
    Ok(())
}
