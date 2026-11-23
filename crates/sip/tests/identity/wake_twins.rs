use super::{answered, authenticated_invite, config, incoming_id, outbound, provisional};
use crate::twins::Twins;
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
fn a_ringing_call_that_times_out_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let mut twins = Twins::new(calling)?;
    let invite = twins
        .apply(|engine, now| engine.originate(outbound(), now))?
        .datagrams
        .remove(0)
        .bytes;
    twins.advance(700)?;
    let ringing = provisional(&invite)?;
    twins.apply(|engine, now| engine.receive(&ringing, "127.0.0.1:5061".parse()?, now))?;
    let output = twins.advance(31_000)?;
    assert!(output
        .datagrams
        .iter()
        .any(|datagram| datagram.bytes.starts_with(b"CANCEL ")));
    let late = answered(&invite)?;
    twins.apply(|engine, now| engine.receive(&late, "127.0.0.1:5061".parse()?, now))?;
    twins.advance(110_000)?;
    assert_eq!(twins.retained(), rusterisk_sip::Retained::default());
    Ok(())
}
