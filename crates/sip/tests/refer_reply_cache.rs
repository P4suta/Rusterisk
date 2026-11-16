use rusterisk_sip::{ReferProgress, SipError, SipEvent};
#[path = "refer_dialog/support.rs"]
mod support;
use support::{notifies, receipt, refer, status, Fixture};

#[test]
fn cached_acceptance_keeps_its_identity_and_expiry_after_notification_completion(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let identity = fixture
        .engine
        .refer_identity(requested.key())
        .ok_or(SipError::InvalidPacket)?;
    assert_eq!(fixture.engine.refer_reply_deadline(&identity)?, None);
    let accepted = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
    let initial = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&initial, 200)?, fixture.peer, 7)?;
    let finished = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Answered, 8)?;
    let final_notify = notifies(&finished)?.pop().ok_or(SipError::InvalidPacket)?;
    let settled = fixture
        .engine
        .receive(&receipt(&final_notify, 200)?, fixture.peer, 9)?;
    assert!(settled.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == requested.key())));
    assert!(fixture.engine.refer_identity(requested.key()).is_none());
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_006)
    );
    let replay = fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 10)?;
    assert!(status(&replay, 202));
    assert!(replay.events.is_empty());
    assert!(notifies(&replay)?.is_empty());
    assert!(identity.matches(replay.datagrams.first().ok_or(SipError::InvalidPacket)?)?);
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_006)
    );
    let registered = fixture.receive("REGISTER", 3, "", 10_000)?;
    assert_eq!(registered.datagrams.len(), 1);
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_006)
    );
    fixture.engine.tick(32_005)?;
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_006)
    );
    let expired = fixture.engine.tick(32_006)?;
    assert!(!expired.events.iter().any(|event| matches!(
        event,
        SipEvent::Ended { .. } | SipEvent::IncomingEnded { .. }
    )));
    assert_eq!(fixture.engine.refer_reply_deadline(&identity)?, None);
    let register_replay = fixture.receive("REGISTER", 3, "", 32_007)?;
    assert_eq!(
        register_replay
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
        registered
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes
    );
    let digit = rusterisk_core::DtmfDigit::from_event(1).ok_or(SipError::InvalidPacket)?;
    let sent = fixture
        .engine
        .send_info(&fixture.id, 7, digit, 100, 32_008)?;
    let info = support::request(&sent.datagrams.first().ok_or(SipError::InvalidPacket)?.bytes)?;
    fixture
        .engine
        .receive(&receipt(&info, 200)?, fixture.peer, 32_009)?;
    Ok(())
}

#[test]
fn suppressed_replies_use_the_server_transaction_deadline_at_handoff() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested = refer(fixture.receive(
        "REFER",
        3,
        "Refer-To: <sip:200@target.invalid>\r\nRefer-Sub: false\r\n",
        5,
    )?)?;
    let identity = fixture
        .engine
        .refer_identity(requested.key())
        .ok_or(SipError::InvalidPacket)?;
    let trying = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
    assert!(trying.datagrams.is_empty());
    assert_eq!(fixture.engine.refer_reply_deadline(&identity)?, None);
    let handed_off =
        fixture
            .engine
            .refer_progress(requested.key(), ReferProgress::HandedOff, 100)?;
    assert!(status(&handed_off, 200));
    assert!(notifies(&handed_off)?.is_empty());
    assert!(fixture.engine.refer_identity(requested.key()).is_none());
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_100)
    );
    let replay = fixture.receive(
        "REFER",
        3,
        "Refer-To: <sip:200@target.invalid>\r\nRefer-Sub: false\r\n",
        101,
    )?;
    assert!(status(&replay, 200));
    assert!(identity.matches(replay.datagrams.first().ok_or(SipError::InvalidPacket)?)?);
    assert_eq!(
        fixture.engine.refer_reply_deadline(&identity)?,
        Some(32_100)
    );
    fixture.engine.tick(32_100)?;
    assert_eq!(fixture.engine.refer_reply_deadline(&identity)?, None);
    Ok(())
}
