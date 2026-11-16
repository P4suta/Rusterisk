use super::support::{notifies, receipt, status, Fixture};
use rusterisk_sip::{AttendedDialogRefer, Output, ReferProgress, SipEvent};

const TARGET: &str = "Refer-To: <sip:target@localhost?Replaces=Consultation%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote>\r\n";

fn attended(output: Output, expected: &str) -> Result<AttendedDialogRefer, String> {
    assert!(output.datagrams.is_empty());
    let mut events = output.events.into_iter();
    let Some(SipEvent::AttendedTransferRequested {
        id,
        generation: 7,
        refer,
    }) = events.next()
    else {
        return Err("Missing attended transfer request".into());
    };
    assert_eq!(id, expected);
    assert!(events.next().is_none());
    Ok(refer)
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[test]
fn attended_requests_keep_the_source_authority_and_independent_subscription() -> Result<(), String>
{
    let mut fixture = Fixture::new(true, true).map_err(failure)?;
    let requested = attended(
        fixture.receive("REFER", 3, TARGET, 5).map_err(failure)?,
        &fixture.id,
    )?;
    assert_eq!(requested.authority().endpoint(), "alice");
    assert_eq!(requested.authority().context(), "internal");
    assert_eq!(
        requested.authority().authenticated_identity(),
        Some("phone")
    );
    assert_eq!(requested.target().replaces().call_id(), "Consultation");
    assert_eq!(requested.target().replaces().to_tag(), "Local");
    assert_eq!(requested.target().replaces().from_tag(), "Remote");
    let repeated = fixture.receive("REFER", 3, TARGET, 6).map_err(failure)?;
    assert!(repeated.datagrams.is_empty());
    assert!(repeated.events.is_empty());
    let accepted = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 7)
        .map_err(failure)?;
    assert!(status(&accepted, 202));
    let initial = notifies(&accepted)
        .map_err(failure)?
        .pop()
        .ok_or("Missing initial NOTIFY")?;
    assert_eq!(initial.body, b"SIP/2.0 100 Trying\r\n");
    fixture
        .engine
        .receive(&receipt(&initial, 200).map_err(failure)?, fixture.peer, 8)
        .map_err(failure)?;
    let ended = fixture.receive("BYE", 4, "", 9).map_err(failure)?;
    assert!(status(&ended, 200));
    assert!(matches!(
        ended.events.as_slice(),
        [SipEvent::Ended { id, generation: 7, .. }] if id == &fixture.id
    ));
    let completed = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Answered, 10)
        .map_err(failure)?;
    let final_notify = notifies(&completed)
        .map_err(failure)?
        .pop()
        .ok_or("Missing final NOTIFY")?;
    assert_eq!(final_notify.body, b"SIP/2.0 200 OK\r\n");
    fixture
        .engine
        .receive(
            &receipt(&final_notify, 200).map_err(failure)?,
            fixture.peer,
            11,
        )
        .map_err(failure)?;
    let settled = fixture.engine.tick(40_000).map_err(failure)?;
    assert!(settled.datagrams.is_empty());
    assert!(settled.events.is_empty());
    Ok(())
}

#[test]
fn malformed_selectors_and_unsupported_uri_roles_never_fall_back_to_blind_transfer(
) -> Result<(), String> {
    for (target, code) in [
        ("<sip:target@localhost?Replaces=broken>", 400),
        ("<sip:target@localhost?Replaces=consultation%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote%0D%0AInjected>", 400),
        ("<sip:target@localhost;Replaces=consultation%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote>", 501),
        ("<sips:target@localhost?Replaces=consultation%3Bto-tag%3DLocal%3Bfrom-tag%3DRemote>", 501),
    ] {
        let mut fixture = Fixture::new(true, true).map_err(failure)?;
        let output = fixture.receive("REFER", 3, &format!("Refer-To: {target}\r\n"), 5).map_err(failure)?;
        assert!(status(&output, code), "{target}");
        assert!(output.events.is_empty());
        assert!(notifies(&output).map_err(failure)?.is_empty());
        assert!(status(&fixture.receive("BYE", 4, "", 6).map_err(failure)?, 200));
    }
    Ok(())
}

#[test]
fn attended_notification_suppression_does_not_claim_a_new_peer_answer() -> Result<(), String> {
    let mut fixture = Fixture::new(true, true).map_err(failure)?;
    let requested = attended(
        fixture
            .receive("REFER", 3, &format!("{TARGET}Refer-Sub: false\r\n"), 5)
            .map_err(failure)?,
        &fixture.id,
    )?;
    let trying = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)
        .map_err(failure)?;
    assert!(trying.datagrams.is_empty());
    assert!(trying.events.is_empty());
    let completed = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Answered, 7)
        .map_err(failure)?;
    assert!(status(&completed, 200));
    assert!(notifies(&completed).map_err(failure)?.is_empty());
    assert!(
        matches!(completed.events.as_slice(), [SipEvent::ReferSubscriptionEnded { refer }] if *refer == requested.key())
    );
    let debug = format!("{requested:?}");
    for private in ["Consultation", "Local", "Remote", "phone"] {
        assert!(!debug.contains(private));
    }
    Ok(())
}
