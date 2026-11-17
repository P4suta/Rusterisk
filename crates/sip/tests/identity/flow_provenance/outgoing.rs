use super::{answer, engine, first, originate, request, scoped, settings, PEER};
use rsipstack::sip::{HeadersExt, Method};
use rusterisk_config::Protocol;
use rusterisk_sip::{DatagramOrigin, Engine, SipError, SipEvent, StreamFlowId};

#[test]
fn only_the_prepared_flow_can_challenge_ring_or_answer_its_invitation() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(301)?;
        let another = StreamFlowId::new(302)?;
        let offered = originate(&mut engine, protocol, original, "flow-outgoing", 7, 0)?;
        let challenge = super::super::challenge_invite(&first(&offered)?.bytes, false)?;
        let foreign = engine.receive_on_flow(&challenge, PEER, another, 1)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        let retried = engine.receive_on_flow(&challenge, PEER, original, 2)?;
        scoped(&retried, original);
        let [ack, invited] = retried.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        assert!(request(&invited.bytes)?.authorization_header().is_some());
        assert!(retried.events.is_empty());
        let ringing = super::super::provisional(&invited.bytes)?;
        let foreign = engine.receive_on_flow(&ringing, PEER, another, 3)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        let owned = engine.receive_on_flow(&ringing, PEER, original, 4)?;
        assert!(
            matches!(owned.events.as_slice(), [SipEvent::Ringing { id, generation: 7 }] if id == "flow-outgoing")
        );
        let answered = answer(protocol, &invited.bytes, "callee")?;
        let foreign = engine.receive_on_flow(&answered, PEER, another, 5)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        let owned = engine.receive_on_flow(&answered, PEER, original, 6)?;
        scoped(&owned, original);
        assert_eq!(request(&first(&owned)?.bytes)?.method, Method::Ack);
        assert!(
            matches!(owned.events.as_slice(), [SipEvent::Answered { id, generation: 7, .. }] if id == "flow-outgoing")
        );
    }
    Ok(())
}

#[test]
fn fork_cleanup_and_its_retained_ack_keep_the_winner_flow() -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(401)?;
        let another = StreamFlowId::new(402)?;
        let offered = originate(&mut engine, protocol, original, "fork-winner", 7, 0)?;
        let invitation = &first(&offered)?.bytes;
        let winner =
            engine.receive_on_flow(&answer(protocol, invitation, "winner")?, PEER, original, 1)?;
        scoped(&winner, original);
        let fork = answer(protocol, invitation, "unselected")?;
        let foreign = engine.receive_on_flow(&fork, PEER, another, 2)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        let cleanup = engine.receive_on_flow(&fork, PEER, original, 3)?;
        scoped(&cleanup, original);
        assert!(cleanup.events.is_empty());
        let [ack, bye] = cleanup.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        assert_eq!(request(&bye.bytes)?.method, Method::Bye);
        assert_eq!(ack.origin(), DatagramOrigin::Direct);
        assert_eq!(bye.origin(), DatagramOrigin::Direct);
        let replayed = engine.receive_on_flow(&fork, PEER, original, 4)?;
        scoped(&replayed, original);
        assert_eq!(replayed.datagrams.len(), 1);
        assert!(first(&replayed)?.bytes == ack.bytes);
        let foreign = engine.receive_on_flow(&fork, PEER, another, 5)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        assert!(engine.active_session("fork-winner", 7).is_some());
        let ended = engine.cancel("fork-winner", 7, 6)?;
        scoped(&ended, original);
        assert_eq!(request(&first(&ended)?.bytes)?.method, Method::Bye);
    }
    Ok(())
}

#[test]
fn a_cancelled_generation_keeps_late_cleanup_on_its_flow_after_channel_reuse(
) -> Result<(), SipError> {
    for protocol in [Protocol::Tcp, Protocol::Tls] {
        let mut engine = engine(protocol)?;
        let original = StreamFlowId::new(501)?;
        let another = StreamFlowId::new(502)?;
        let offered = originate(&mut engine, protocol, original, "reused", 7, 0)?;
        let invitation = &first(&offered)?.bytes;
        engine.receive_on_flow(&super::super::provisional(invitation)?, PEER, original, 1)?;
        let cancelled = engine.cancel("reused", 7, 2)?;
        scoped(&cancelled, original);
        assert_eq!(request(&first(&cancelled)?.bytes)?.method, Method::Cancel);
        let replacement = originate(&mut engine, protocol, another, "reused", 8, 3)?;
        let late = answer(protocol, invitation, "late")?;
        let foreign = engine.receive_on_flow(&late, PEER, another, 4)?;
        assert!(foreign.datagrams.is_empty());
        assert!(foreign.events.is_empty());
        let cleanup = engine.receive_on_flow(&late, PEER, original, 5)?;
        scoped(&cleanup, original);
        assert!(cleanup.events.is_empty());
        let [ack, bye] = cleanup.datagrams.as_slice() else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(request(&ack.bytes)?.method, Method::Ack);
        assert_eq!(request(&bye.bytes)?.method, Method::Bye);
        let answered = engine.receive_on_flow(
            &answer(protocol, &first(&replacement)?.bytes, "replacement")?,
            PEER,
            another,
            6,
        )?;
        scoped(&answered, another);
        assert!(
            matches!(answered.events.as_slice(), [SipEvent::Answered { id, generation: 8, .. }] if id == "reused")
        );
        let replayed = engine.receive_on_flow(&late, PEER, original, 7)?;
        scoped(&replayed, original);
        assert_eq!(replayed.datagrams.len(), 1);
        assert!(first(&replayed)?.bytes == ack.bytes);
        assert!(engine.active_session("reused", 8).is_some());
        let ended = engine.cancel("reused", 8, 8)?;
        scoped(&ended, another);
    }
    Ok(())
}

#[test]
fn legacy_single_flow_entrypoints_leave_flow_metadata_absent() -> Result<(), SipError> {
    let protocol = Protocol::Tcp;
    let mut engine = Engine::new(settings(protocol)?, [19; 32]).with_transport(protocol);
    let offered = engine.originate(super::super::outbound(), 0)?;
    assert_eq!(first(&offered)?.flow(), None);
    let answered = engine.receive(
        &answer(protocol, &first(&offered)?.bytes, "legacy")?,
        PEER,
        1,
    )?;
    assert_eq!(first(&answered)?.flow(), None);
    let ended = engine.disconnected(PEER);
    assert!(
        matches!(ended.events.as_slice(), [SipEvent::Ended { id, generation: 7, cause: 41 }] if id == "outgoing")
    );
    Ok(())
}
