use super::{answered, config, outbound, provisional, request};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method};
use rusterisk_sip::{Datagram, Engine, SipError, SipEvent};
#[path = "forked_answers/boundaries.rs"]
mod boundaries;

fn forked(invite: &[u8], tag: &str) -> Result<Vec<u8>, SipError> {
    let response = answered(invite)?;
    Ok(std::str::from_utf8(&response)?
        .replace("tag=callee", &format!("tag={tag}"))
        .into_bytes())
}

fn cleanup_requests(packets: &[Datagram], tag: &str) -> Result<(), SipError> {
    assert_eq!(packets.len(), 2, "fork {tag} must receive its ACK and BYE");
    let [ack, bye] = packets else {
        return Err(SipError::InvalidPacket);
    };
    let ack = request(&ack.bytes)?;
    let bye = request(&bye.bytes)?;
    assert_eq!(ack.method, Method::Ack);
    assert_eq!(bye.method, Method::Bye);
    assert_eq!(ack.cseq_header()?.typed()?.seq, 1);
    assert_eq!(bye.cseq_header()?.typed()?.seq, 2);
    for packet in [&ack, &bye] {
        assert_eq!(
            packet.to_header()?.typed()?.tag().map(|tag| tag.value()),
            Some(tag)
        );
    }
    assert_ne!(
        ack.top_via_header()?.typed()?.branch(),
        bye.top_via_header()?.typed()?.branch()
    );
    Ok(())
}

#[test]
fn an_additional_answer_is_acknowledged_and_terminated_without_replacing_the_winner(
) -> Result<(), SipError> {
    let mut settings = config()?;
    settings.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(settings, [9; 32]);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let peer = "127.0.0.1:5061".parse()?;
    let winner = engine.receive(&answered(&invite)?, peer, 1)?;
    assert!(matches!(
        winner.events.as_slice(),
        [SipEvent::Answered { generation: 7, .. }]
    ));
    let additional = engine.receive(&forked(&invite, "other")?, peer, 2)?;
    assert!(additional.events.is_empty());
    cleanup_requests(&additional.datagrams, "other")?;
    assert!(engine.active_session("outgoing", 7).is_some());
    let ended = engine.cancel("outgoing", 7, 3)?;
    let [bye] = ended.datagrams.as_slice() else {
        return Err(SipError::InvalidPacket);
    };
    let bye = request(&bye.bytes)?;
    assert_eq!(bye.method, Method::Bye);
    assert_eq!(
        bye.to_header()?.typed()?.tag().map(|tag| tag.value()),
        Some("callee")
    );
    Ok(())
}

#[test]
fn cancellation_acknowledges_and_terminates_each_late_answer_dialog() -> Result<(), SipError> {
    let mut settings = config()?;
    settings.aors[0].contact = Some("sip:phone@127.0.0.1:5061".into());
    let mut engine = Engine::new(settings, [9; 32]);
    let invite = engine.originate(outbound(), 0)?.datagrams.remove(0).bytes;
    let peer = "127.0.0.1:5061".parse()?;
    engine.receive(&provisional(&invite)?, peer, 1)?;
    engine.cancel("outgoing", 7, 2)?;
    let initial = engine.receive(&answered(&invite)?, peer, 3)?;
    assert!(initial.events.is_empty());
    cleanup_requests(&initial.datagrams, "callee")?;
    let additional = engine.receive(&forked(&invite, "other")?, peer, 4)?;
    assert!(additional.events.is_empty());
    cleanup_requests(&additional.datagrams, "other")?;
    assert!(engine.active_session("outgoing", 7).is_none());
    Ok(())
}
