use rsipstack::sip::Method;
use rusterisk_sip::{DatagramOrigin, ReferProgress, SipError};
#[path = "refer_dialog/support.rs"]
mod support;
use support::{notifies, receipt, refer, status, Fixture};

fn refer_packet(fixture: &Fixture, branch: Option<&str>) -> Result<String, SipError> {
    let original =
        String::from_utf8(fixture.packet("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n")?)
            .map_err(|_| SipError::InvalidPacket)?
            .replacen(
                "SIP/2.0/UDP 127.0.0.1:5061",
                "SIP/2.0/UDP Phone.Example:5061",
                1,
            );
    Ok(match branch {
        Some(branch) => original.replace("branch=z9hG4bK-REFER-3", &format!("branch={branch}")),
        None => original.replace(";branch=z9hG4bK-REFER-3", ""),
    })
}

fn check(branch: Option<&str>) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let original = refer_packet(&fixture, branch)?;
    let requested = refer(
        fixture
            .engine
            .receive(original.as_bytes(), fixture.peer, 5)?,
    )?;
    let identity = fixture
        .engine
        .refer_identity(requested.key())
        .ok_or(SipError::InvalidPacket)?;
    let accepted = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
    let initial = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&initial, 200)?, fixture.peer, 7)?;
    let response = accepted
        .datagrams
        .iter()
        .find(|packet| packet.bytes.starts_with(b"SIP/2.0 202 "))
        .ok_or(SipError::InvalidPacket)?;
    assert!(identity.matches(response)?);
    let mut normalized = response.clone();
    normalized.bytes = std::str::from_utf8(&response.bytes)?
        .replace("Phone.Example", "PHONE.EXAMPLE")
        .into_bytes();
    assert!(identity.matches(&normalized)?);
    let wrong_branch = match branch {
        Some(branch) => original.replace(&format!("branch={branch}"), "branch=z9hG4bK-other"),
        None => original.replace(";rport", ";branch=z9hG4bK-other;rport"),
    };
    let wrong_sent_by = original.replace("Phone.Example:5061", "Other.Example:5061");
    for changed in [wrong_branch, wrong_sent_by] {
        let rejected = fixture
            .engine
            .receive(changed.as_bytes(), fixture.peer, 8)?;
        assert!(status(&rejected, 500));
        assert!(rejected.events.is_empty());
        let rejected = rejected.datagrams.first().ok_or(SipError::InvalidPacket)?;
        assert!(!identity.matches(rejected)?);
    }
    let replay = fixture
        .engine
        .receive(original.as_bytes(), fixture.peer, 9)?;
    assert!(status(&replay, 202));
    assert!(notifies(&replay)?.is_empty());
    assert!(identity.matches(replay.datagrams.first().ok_or(SipError::InvalidPacket)?)?);
    let ended = fixture.engine.cancel(&fixture.id, 7, 10)?;
    let bye = support::request(
        &ended
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .bytes,
    )?;
    assert_eq!(bye.method, Method::Bye);
    Ok(())
}

#[test]
fn cookie_branch_and_sent_by_belong_to_the_original_refer_transaction() -> Result<(), SipError> {
    check(Some("z9hG4bK-original"))
}

#[test]
fn legacy_vias_keep_their_original_branch_presence_and_sent_by() -> Result<(), SipError> {
    check(None)?;
    check(Some("legacy-original"))
}

#[test]
fn legacy_request_uri_changes_cannot_inherit_an_earlier_reply_route() -> Result<(), SipError> {
    for branch in [None, Some("legacy-original")] {
        let mut fixture = Fixture::new(true, true)?;
        let original = refer_packet(&fixture, branch)?;
        let requested = refer(
            fixture
                .engine
                .receive(original.as_bytes(), fixture.peer, 5)?,
        )?;
        let identity = fixture
            .engine
            .refer_identity(requested.key())
            .ok_or(SipError::InvalidPacket)?;
        let accepted = fixture
            .engine
            .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
        assert!(status(&accepted, 202));
        let changed = original.replacen(
            "REFER sip:100@localhost SIP/2.0",
            "REFER sip:another@localhost SIP/2.0",
            1,
        );
        let rejected = fixture
            .engine
            .receive(changed.as_bytes(), fixture.peer, 7)?;
        assert!(status(&rejected, 481));
        assert!(rejected.events.is_empty());
        assert!(!identity.matches(rejected.datagrams.first().ok_or(SipError::InvalidPacket)?)?);
    }
    Ok(())
}

#[test]
fn engine_provenance_survives_cached_replies_and_notification_retries() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let key = requested.key();
    let accepted = fixture
        .engine
        .refer_progress(key, ReferProgress::Trying, 6)?;
    assert_eq!(accepted.datagrams.len(), 2);
    assert!(accepted
        .datagrams
        .iter()
        .all(|packet| packet.origin() == DatagramOrigin::Refer(key)));
    let retried = fixture.engine.tick(506)?;
    let mut references = 0;
    for packet in retried.datagrams {
        match packet.origin() {
            DatagramOrigin::Refer(reference) => {
                assert_eq!(reference, key);
                assert_eq!(support::request(&packet.bytes)?.method, Method::Notify);
                references += 1;
            }
            DatagramOrigin::Direct => assert!(packet.bytes.starts_with(b"SIP/2.0 401 ")),
        }
    }
    assert_eq!(references, 1);
    let replay = fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 507)?;
    assert!(status(&replay, 202));
    assert_eq!(
        replay
            .datagrams
            .first()
            .ok_or(SipError::InvalidPacket)?
            .origin(),
        DatagramOrigin::Refer(key)
    );
    let ended = fixture.engine.cancel(&fixture.id, 7, 508)?;
    assert!(ended
        .datagrams
        .iter()
        .all(|packet| packet.origin() == DatagramOrigin::Direct));
    Ok(())
}

#[test]
fn deferred_status_and_subscription_notifications_keep_their_original_reference(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let key = requested.key();
    let accepted = fixture
        .engine
        .refer_progress(key, ReferProgress::Trying, 6)?;
    let initial = notifies(&accepted)?.pop().ok_or(SipError::InvalidPacket)?;
    fixture
        .engine
        .receive(&receipt(&initial, 200)?, fixture.peer, 7)?;
    assert!(fixture
        .engine
        .refer_progress(key, ReferProgress::Ringing, 8)?
        .datagrams
        .is_empty());
    let deferred = fixture.engine.tick(1006)?;
    assert_eq!(
        deferred
            .datagrams
            .iter()
            .filter(|packet| packet.origin() == DatagramOrigin::Refer(key))
            .count(),
        1
    );
    let unsubscribed =
        fixture.receive("SUBSCRIBE", 4, "Event: refer;id=3\r\nExpires: 0\r\n", 1007)?;
    assert_eq!(unsubscribed.datagrams.len(), 2);
    for packet in unsubscribed.datagrams {
        match packet.origin() {
            DatagramOrigin::Direct => assert!(packet.bytes.starts_with(b"SIP/2.0 200 ")),
            DatagramOrigin::Refer(reference) => {
                assert_eq!(reference, key);
                assert_eq!(support::request(&packet.bytes)?.method, Method::Notify);
            }
        }
    }
    Ok(())
}
