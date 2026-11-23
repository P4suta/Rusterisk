use super::support::{notifies, receipt, refer, status, Fixture};
use crate::twins::Twins;
use rusterisk_sip::{Engine, ReferProgress, SipError, SipEvent};

const TARGET: &str = "Refer-To: <sip:200@target.invalid>\r\n";

fn established() -> Result<Engine, SipError> {
    Fixture::new(true, true).map(|fixture| fixture.engine)
}

fn ended(output: &rusterisk_sip::Output) -> bool {
    output
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { .. }))
}

#[test]
fn an_unanswered_transfer_request_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let fixture = Fixture::new(true, true)?;
    let mut twins = Twins::new(established)?;
    twins.advance(5)?;
    let packet = fixture.packet("REFER", 3, TARGET)?;
    twins.apply(|engine, now| engine.receive(&packet, fixture.peer, now))?;
    assert_eq!(twins.retained().refers, 1);
    let output = twins.advance(40_000)?;
    assert!(status(&output, 503));
    assert!(ended(&output));
    twins.advance(80_000)?;
    assert_eq!(twins.retained().refers, 0);
    assert!(twins.skipped() > 79_900);
    Ok(())
}

#[test]
fn a_transfer_subscription_changes_only_at_its_reported_wakes() -> Result<(), SipError> {
    let fixture = Fixture::new(true, true)?;
    let mut twins = Twins::new(established)?;
    twins.advance(5)?;
    let packet = fixture.packet("REFER", 3, TARGET)?;
    let request = refer(twins.apply(|engine, now| engine.receive(&packet, fixture.peer, now))?)?;
    twins.advance(6)?;
    let accepted = twins
        .apply(|engine, now| engine.refer_progress(request.key(), ReferProgress::Trying, now))?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    twins.advance(7)?;
    let acknowledged = receipt(&notification, 200)?;
    twins.apply(|engine, now| engine.receive(&acknowledged, fixture.peer, now))?;
    twins.advance(300)?;
    let limited = twins
        .apply(|engine, now| engine.refer_progress(request.key(), ReferProgress::Ringing, now))?;
    assert!(limited.datagrams.is_empty());
    let ringing = twins.advance(2_000)?;
    let notification = notifies(&ringing)?.pop().ok_or(SipError::InvalidPacket)?;
    assert_eq!(notification.body, b"SIP/2.0 180 Ringing\r\n");
    let acknowledged = receipt(&notification, 200)?;
    twins.apply(|engine, now| engine.receive(&acknowledged, fixture.peer, now))?;
    let bye = fixture.packet("BYE", 4, "")?;
    let hung_up = twins.apply(|engine, now| engine.receive(&bye, fixture.peer, now))?;
    assert!(status(&hung_up, 200));
    twins.advance(100_000)?;
    assert_eq!(twins.retained().incoming, 1);
    let output = twins.advance(680_000)?;
    assert!(!notifies(&output)?.is_empty());
    assert!(ended(&output));
    assert_eq!(twins.retained().refers, 0);
    assert_eq!(twins.retained().incoming, 0);
    assert!(twins.skipped() > 679_000);
    Ok(())
}
