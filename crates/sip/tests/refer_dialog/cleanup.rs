use super::support::{notifies, receipt, refer, status, Fixture};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt};
use rusterisk_sip::{Output, ReferKey, ReferProgress, SipError, SipEvent};

enum RefreshRejection {
    Response(u16),
    Packet,
}

fn closed(output: &Output, expected: ReferKey) -> bool {
    output.events.iter().any(
        |event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == expected),
    )
}

fn no_call_end(output: &Output) -> bool {
    !output.events.iter().any(|event| {
        matches!(
            event,
            SipEvent::Ended { .. } | SipEvent::IncomingEnded { .. }
        )
    })
}

#[test]
fn notify_rejection_checks_the_transaction_and_only_closes_its_subscription() -> Result<(), SipError>
{
    let mut fixture = Fixture::new(true, true)?;
    let first = refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(first.key(), ReferProgress::Trying, 6)?;
    let first_notify = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    let second =
        refer(fixture.receive("REFER", 4, "Refer-To: <sip:300@target.invalid>\r\n", 7)?)?;
    let accepted = fixture
        .engine
        .refer_progress(second.key(), ReferProgress::Trying, 8)?;
    let second_notify = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&second_notify, 200)?, fixture.peer, 9)?;
    let wrong_peer = "127.0.0.1:5062".parse()?;
    let unmatched = fixture
        .engine
        .receive(&receipt(&first_notify, 481)?, wrong_peer, 10)?;
    assert!(!closed(&unmatched, first.key()));
    let mut wrong_tag = first_notify.clone();
    let changed = wrong_tag.to_header()?.typed()?.with_tag("unrelated".into());
    wrong_tag.to_header_mut()?.replace(changed.to_string());
    let unmatched = fixture
        .engine
        .receive(&receipt(&wrong_tag, 481)?, fixture.peer, 11)?;
    assert!(!closed(&unmatched, first.key()));
    let rejected = fixture
        .engine
        .receive(&receipt(&first_notify, 481)?, fixture.peer, 12)?;
    assert!(closed(&rejected, first.key()));
    assert!(!closed(&rejected, second.key()));
    assert!(no_call_end(&rejected));
    assert!(fixture
        .engine
        .refer_progress(first.key(), ReferProgress::Ringing, 2000)?
        .datagrams
        .is_empty());
    let ringing = fixture
        .engine
        .refer_progress(second.key(), ReferProgress::Ringing, 2000)?;
    assert_eq!(
        notifies(&ringing)?
            .first()
            .ok_or(SipError::InvalidPacket)?
            .body,
        b"SIP/2.0 180 Ringing\r\n"
    );
    let info = fixture.receive("INFO", 5, "", 2001)?;
    assert!(status(&info, 200));
    assert!(no_call_end(&info));
    Ok(())
}

#[test]
fn notify_timer_f_releases_the_subscription_without_ending_the_call() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    let timed_out = fixture.engine.tick(32_006)?;
    assert!(closed(&timed_out, request.key()));
    assert!(no_call_end(&timed_out));
    assert!(timed_out.datagrams.is_empty());
    assert!(fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Answered, 32_007)?
        .datagrams
        .is_empty());
    let info = fixture.receive("INFO", 4, "", 32_008)?;
    assert!(status(&info, 200));
    assert!(no_call_end(&info));
    Ok(())
}

#[test]
fn unsubscribe_only_terminates_the_matching_refer_usage() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let first = refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(first.key(), ReferProgress::Trying, 6)?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 7)?;
    let second =
        refer(fixture.receive("REFER", 4, "Refer-To: <sip:300@target.invalid>\r\n", 8)?)?;
    let accepted = fixture
        .engine
        .refer_progress(second.key(), ReferProgress::Trying, 9)?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 10)?;
    let unsubscribed =
        fixture.receive("SUBSCRIBE", 5, "Event: refer;id=3\r\nExpires: 0\r\n", 11)?;
    assert!(status(&unsubscribed, 200));
    assert!(no_call_end(&unsubscribed));
    let notification = notifies(&unsubscribed)?
        .pop()
        .ok_or(SipError::InvalidPacket)?;
    assert_eq!(notification.body, b"SIP/2.0 100 Trying\r\n");
    assert!(notification.headers.iter().any(|header| header
        .name()
        .eq_ignore_ascii_case("Subscription-State")
        && header.value() == "terminated;reason=noresource"));
    let completed = fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 12)?;
    assert!(closed(&completed, first.key()));
    assert!(!closed(&completed, second.key()));
    assert!(fixture
        .engine
        .refer_progress(first.key(), ReferProgress::Answered, 13)?
        .datagrams
        .is_empty());
    assert_eq!(
        notifies(
            &fixture
                .engine
                .refer_progress(second.key(), ReferProgress::Ringing, 1010)?
        )?
        .first()
        .ok_or(SipError::InvalidPacket)?
        .body,
        b"SIP/2.0 180 Ringing\r\n"
    );
    assert!(status(&fixture.receive("INFO", 6, "", 1011)?, 200));
    Ok(())
}

#[test]
fn subscription_refresh_outlives_bye_and_announces_the_current_status() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 7)?;
    assert!(status(&fixture.receive("BYE", 4, "", 8)?, 200));
    assert!(fixture.engine.tick(40_000)?.datagrams.is_empty());
    let refreshed = fixture.receive(
        "SUBSCRIBE",
        5,
        "Event: refer;id=3\r\nExpires: 120\r\n",
        40_001,
    )?;
    assert!(status(&refreshed, 200));
    let notification = notifies(&refreshed)?.pop().ok_or(SipError::InvalidPacket)?;
    assert_eq!(notification.body, b"SIP/2.0 100 Trying\r\n");
    assert!(notification.headers.iter().any(|header| header
        .name()
        .eq_ignore_ascii_case("Subscription-State")
        && header.value() == "active;expires=120"));
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 40_002)?;
    assert!(fixture.engine.tick(160_000)?.datagrams.is_empty());
    let expired = fixture.engine.tick(160_001)?;
    assert_eq!(
        notifies(&expired)?
            .first()
            .ok_or(SipError::InvalidPacket)?
            .body,
        b"SIP/2.0 100 Trying\r\n"
    );
    assert!(no_call_end(&expired));
    Ok(())
}

#[test]
fn pending_deadline_and_peer_close_have_explicit_cleanup() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let pending =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let expired = fixture.engine.tick(32_005)?;
    assert!(status(&expired, 503));
    assert!(closed(&expired, pending.key()));
    assert!(no_call_end(&expired));
    assert!(fixture
        .engine
        .refer_progress(pending.key(), ReferProgress::Trying, 32_006)?
        .datagrams
        .is_empty());
    let active =
        refer(fixture.receive("REFER", 4, "Refer-To: <sip:200@target.invalid>\r\n", 32_007)?)?;
    fixture
        .engine
        .refer_progress(active.key(), ReferProgress::Trying, 32_008)?;
    let unrelated = fixture.engine.disconnected("127.0.0.1:5062".parse()?);
    assert!(!closed(&unrelated, active.key()));
    let disconnected = fixture.engine.disconnected(fixture.peer);
    assert!(closed(&disconnected, active.key()));
    assert!(fixture
        .engine
        .refer_progress(active.key(), ReferProgress::Answered, 32_009)?
        .datagrams
        .is_empty());
    assert!(fixture.engine.tick(650_000)?.datagrams.is_empty());
    Ok(())
}

#[test]
fn owner_capacity_rejects_before_allocation_and_keys_are_not_reused_after_release(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let mut previous = Vec::new();
    for sequence in 3..67 {
        previous.push(
            refer(fixture.receive(
                "REFER",
                sequence,
                "Refer-To: <sip:200@target.invalid>\r\n",
                5,
            )?)?
            .key(),
        );
    }
    let full = fixture.receive("REFER", 67, "Refer-To: <sip:200@target.invalid>\r\n", 6)?;
    assert!(status(&full, 503));
    assert!(full.events.is_empty());
    let expired = fixture.engine.tick(32_005)?;
    assert_eq!(
        expired
            .events
            .iter()
            .filter(|event| matches!(event, SipEvent::ReferSubscriptionEnded { .. }))
            .count(),
        64
    );
    let admitted = refer(fixture.receive(
        "REFER",
        68,
        "Refer-To: <sip:200@target.invalid>\r\n",
        32_006,
    )?)?;
    assert!(!previous.contains(&admitted.key()));
    assert!(fixture
        .engine
        .refer_progress(previous[0], ReferProgress::Answered, 32_007)?
        .datagrams
        .is_empty());
    Ok(())
}

#[test]
fn notify_error_codes_preserve_other_dialog_usages_and_keep_non_terminating_errors_live(
) -> Result<(), SipError> {
    for (code, terminates) in [
        (404, true),
        (405, true),
        (410, true),
        (416, true),
        (480, true),
        (481, true),
        (482, true),
        (483, true),
        (484, true),
        (485, true),
        (489, true),
        (501, true),
        (604, true),
        (500, false),
        (503, false),
    ] {
        let mut fixture = Fixture::new(true, true)?;
        let request =
            refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
        let accepted = fixture
            .engine
            .refer_progress(request.key(), ReferProgress::Trying, 6)?;
        let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
        let received = fixture
            .engine
            .receive(&receipt(&notification, code)?, fixture.peer, 7)?;
        assert_eq!(closed(&received, request.key()), terminates, "{code}");
        assert!(no_call_end(&received));
        let ringing = fixture
            .engine
            .refer_progress(request.key(), ReferProgress::Ringing, 2000)?;
        assert_eq!(
            notifies(&ringing)?.len(),
            usize::from(!terminates),
            "{code}"
        );
        assert!(status(&fixture.receive("INFO", 4, "", 2001)?, 200));
    }
    Ok(())
}

#[test]
fn malformed_or_unmatched_refreshes_preserve_the_subscription() -> Result<(), SipError> {
    use RefreshRejection::{Packet, Response};
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 7)?;
    for (offset, (headers, expected)) in [
        ("Event: presence\r\nExpires: 0\r\n", Response(489)),
        ("Event: refer\r\nExpires: 0\r\n", Response(481)),
        ("Event: refer;id=003\r\nExpires: 0\r\n", Response(481)),
        ("Event: refer;id=3;id=3\r\nExpires: 0\r\n", Response(400)),
        ("Event: refer;id=3;\r\nExpires: 0\r\n", Response(400)),
        ("Event: refer;id=999\r\nExpires: 0\r\n", Response(481)),
        (
            "Event: refer;id=3\r\nEvent: refer;id=3\r\nExpires: 0\r\n",
            Response(400),
        ),
        ("Event: refer;id=3\r\nExpires: -1\r\n", Packet),
        ("Event: refer;id=3\r\nExpires: +1\r\n", Response(400)),
        ("Event: refer;id=3\r\nExpires: 4294967296\r\n", Packet),
        (
            "Event: refer;id=3\r\nExpires: 0\r\nExpires: 1\r\n",
            Response(400),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let sequence = u32::try_from(offset).map_err(|_| SipError::Capacity)? + 4;
        let packet = fixture.packet("SUBSCRIBE", sequence, headers)?;
        let parsed = rsipstack::sip::SipMessage::try_from(packet.as_slice());
        let received = fixture.receive("SUBSCRIBE", sequence, headers, u64::from(sequence) + 5);
        match expected {
            Response(code) => {
                assert!(parsed.is_ok(), "foreign parse for sequence {sequence}");
                let rejected = received?;
                assert!(status(&rejected, code), "{headers}");
                assert!(notifies(&rejected)?.is_empty());
                assert!(no_call_end(&rejected));
            }
            Packet => {
                assert!(parsed.is_err(), "foreign parse for sequence {sequence}");
                assert!(matches!(received, Err(SipError::Parse(_))));
            }
        }
    }
    let received = fixture.receive(
        "SUBSCRIBE",
        15,
        "Event: refer;id=3;extension=\"semi;colon\"\r\nExpires: 60\r\n",
        30,
    );
    assert!(
        received.is_ok(),
        "quoted extension: {:?}",
        received.as_ref().err()
    );
    let refreshed = received?;
    assert!(status(&refreshed, 200));
    assert_eq!(notifies(&refreshed)?.len(), 1);
    Ok(())
}

#[test]
fn a_subscription_target_refresh_updates_notify_routing_and_rejects_ambiguous_contacts(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let request =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Trying, 6)?;
    let notification = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 7)?;
    let packet = fixture.packet("SUBSCRIBE", 4, "Event: refer;id=3\r\nExpires: 120\r\n")?;
    let packet = std::str::from_utf8(&packet)?.replace(
        "Contact: <sip:phone@127.0.0.1:5061>",
        "Contact: <sip:updated@127.0.0.1:5061>",
    );
    let refreshed = fixture.engine.receive(packet.as_bytes(), fixture.peer, 8)?;
    assert!(status(&refreshed, 200));
    let notification = notifies(&refreshed)?.pop().ok_or(SipError::InvalidPacket)?;
    assert_eq!(notification.uri.to_string(), "sip:updated@127.0.0.1:5061");
    fixture
        .engine
        .receive(&receipt(&notification, 200)?, fixture.peer, 9)?;
    let ambiguous = fixture.receive(
        "SUBSCRIBE",
        5,
        "Event: refer;id=3\r\nExpires: 0\r\nContact: <sip:other@127.0.0.1:5061>\r\n",
        10,
    )?;
    assert!(status(&ambiguous, 400));
    assert!(notifies(&ambiguous)?.is_empty());
    for (index, (target, code)) in [
        ("sip:updated@127.0.0.1:5061;method=INVITE", 501),
        ("sip:updated@127.0.0.1:5061?Subject=ignored", 501),
        ("sip:updated:private-password@127.0.0.1:5061", 501),
        ("sip:updated%0d%0a@127.0.0.1:5061", 400),
        ("sip:updated@[::1]suffix", 400),
        ("sip:updated@peer^invalid", 400),
    ]
    .into_iter()
    .enumerate()
    {
        let sequence = u32::try_from(index).map_err(|_| SipError::Capacity)? + 6;
        let packet =
            fixture.packet("SUBSCRIBE", sequence, "Event: refer;id=3\r\nExpires: 0\r\n")?;
        let packet = std::str::from_utf8(&packet)?.replace(
            "Contact: <sip:phone@127.0.0.1:5061>",
            &format!("Contact: <{target}>"),
        );
        let rejected =
            fixture
                .engine
                .receive(packet.as_bytes(), fixture.peer, u64::from(sequence) + 5)?;
        assert!(status(&rejected, code), "{target}");
        assert!(notifies(&rejected)?.is_empty());
        assert!(no_call_end(&rejected));
    }
    let ringing = fixture
        .engine
        .refer_progress(request.key(), ReferProgress::Ringing, 2000)?;
    assert_eq!(
        notifies(&ringing)?
            .first()
            .ok_or(SipError::InvalidPacket)?
            .uri
            .to_string(),
        "sip:updated@127.0.0.1:5061"
    );
    Ok(())
}
