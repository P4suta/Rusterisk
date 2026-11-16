use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method};
use rusterisk_sip::{ReferFailure, ReferProgress, SipError, SipEvent};
#[path = "refer_dialog/cleanup.rs"]
mod cleanup;
#[path = "refer_dialog/support.rs"]
mod support;
use support::{notifies, receipt, refer, status, Fixture};

#[test]
fn refer_waits_for_application_acceptance_and_keeps_authenticated_authority() -> Result<(), SipError>
{
    let mut fixture = Fixture::new(true, true)?;
    let requested = fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?;
    assert!(requested.datagrams.is_empty());
    let request = refer(requested)?;
    assert_eq!(request.target().extension(), "200");
    assert_eq!(request.authority().endpoint(), "alice");
    assert_eq!(request.authority().context(), "internal");
    assert_eq!(request.authority().authenticated_identity(), Some("phone"));
    let replay = fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 6)?;
    assert!(replay.events.is_empty());
    assert!(replay.datagrams.is_empty());
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 7)?;
    assert!(status(&accepted, 202));
    let notifications = notifies(&accepted)?;
    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    assert_eq!(notification.body, b"SIP/2.0 100 Trying\r\n");
    assert_eq!(
        notification.from_header()?.value(),
        fixture.answer.to_header()?.value()
    );
    assert_eq!(
        notification.to_header()?.value(),
        fixture.answer.from_header()?.value()
    );
    assert_eq!(
        notification.call_id_header()?.value(),
        fixture.answer.call_id_header()?.value()
    );
    assert_eq!(
        notification
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Event"))
            .map(|header| header.value())
            .collect::<Vec<_>>(),
        ["refer;id=3"]
    );
    assert!(notification.headers.iter().any(|header| header
        .name()
        .eq_ignore_ascii_case("Subscription-State")
        && header.value() == "active;expires=600"));
    assert!(notification
        .headers
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("Content-Type")
            && header.value().split(';').next() == Some("message/sipfrag")));
    assert_eq!(
        notification
            .headers
            .iter()
            .find(|header| header.name().eq_ignore_ascii_case("Content-Length"))
            .ok_or(SipError::InvalidPacket)?
            .value()
            .parse::<usize>()
            .map_err(|_| SipError::InvalidPacket)?,
        notification.body.len()
    );
    let replay = fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 8)?;
    assert!(status(&replay, 202));
    assert!(replay.events.is_empty());
    assert!(notifies(&replay)?.is_empty());
    Ok(())
}

#[test]
fn notify_uses_the_dialog_sequence_and_outlives_the_invite_usage() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    let initial = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&initial, 200)?, fixture.peer, 7)?;
    let digit = rusterisk_core::DtmfDigit::from_event(1).ok_or(SipError::InvalidPacket)?;
    let info = fixture.engine.send_info(&fixture.id, 7, digit, 100, 8)?;
    let info = support::request(&info.datagrams.first().ok_or(SipError::InvalidPacket)?.bytes)?;
    assert!(info.cseq_header()?.typed()?.seq > initial.cseq_header()?.typed()?.seq);
    fixture
        .engine
        .receive(&receipt(&info, 200)?, fixture.peer, 9)?;
    let ended = fixture.engine.cancel(&fixture.id, 7, 10)?;
    let bye = support::request(
        &ended
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?;
    assert_eq!(bye.method, Method::Bye);
    assert!(bye.cseq_header()?.typed()?.seq > info.cseq_header()?.typed()?.seq);
    fixture
        .engine
        .receive(&receipt(&bye, 200)?, fixture.peer, 11)?;
    assert!(fixture.engine.tick(40_000)?.datagrams.is_empty());
    let completed =
        fixture
            .engine
            .refer_progress(request.key(), ReferProgress::Answered, 40_001)?;
    let final_notify = notifies(&completed)?.pop().ok_or(SipError::InvalidPacket)?;
    assert_eq!(final_notify.body, b"SIP/2.0 200 OK\r\n");
    assert!(final_notify.cseq_header()?.typed()?.seq > bye.cseq_header()?.typed()?.seq);
    assert!(final_notify.headers.iter().any(|header| header
        .name()
        .eq_ignore_ascii_case("Subscription-State")
        && header.value() == "terminated;reason=noresource"));
    fixture
        .engine
        .receive(&receipt(&final_notify, 200)?, fixture.peer, 40_002)?;
    let repeated = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 40_003)?;
    assert!(repeated.datagrams.is_empty());
    assert!(repeated.events.is_empty());
    assert!(fixture.engine.tick(650_000)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn configured_transfer_denial_and_application_rejection_do_not_accept_a_subscription(
) -> Result<(), SipError> {
    let mut denied = Fixture::new(false, true)?;
    let rejected = denied.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?;
    assert!(status(&rejected, 603));
    assert!(rejected.events.is_empty());
    assert!(notifies(&rejected)?.is_empty());
    assert!(status(&denied.receive("BYE", 4, "", 6)?, 200));
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let rejected = fixture.engine.refer_progress(
        request.key(),
        ReferProgress::Rejected(ReferFailure::Unsupported),
        6,
    )?;
    assert!(status(&rejected, 501));
    assert!(notifies(&rejected)?.is_empty());
    let repeated = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 7)?;
    assert!(repeated.datagrams.is_empty());
    assert!(status(&fixture.receive("BYE", 4, "", 8)?, 200));
    Ok(())
}

#[test]
fn wire_suppression_and_handoff_completion_do_not_claim_a_target_answer() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let request = refer(fixture.receive(
        "REFER",
        3,
        "Refer-To: <sip:200@target.invalid>\r\nRefer-Sub: false\r\n",
        5,
    )?)?;
    let pending = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    assert!(pending.datagrams.is_empty());
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::HandedOff, 7)?;
    assert!(status(&accepted, 200));
    assert!(notifies(&accepted)?.is_empty());
    assert!(accepted.datagrams.iter().any(|packet| packet
        .bytes
        .windows(b"Refer-Sub: false".len())
        .any(|value| value == b"Refer-Sub: false")));
    assert!(!accepted
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Answered { .. })));
    let mut immediate = Fixture::new(true, false)?;
    let request =
        refer(immediate.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = immediate
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    assert!(status(&accepted, 202));
    let completed = immediate
        .engine
        .refer_progress(request.key(), ReferProgress::HandedOff, 7)?;
    assert_eq!(
        notifies(&completed)?
            .first()
            .ok_or(SipError::InvalidPacket)?
            .body,
        b"SIP/2.0 200 OK\r\n"
    );
    assert!(!completed
        .events
        .iter()
        .any(|event| matches!(event, SipEvent::Answered { .. })));
    Ok(())
}
