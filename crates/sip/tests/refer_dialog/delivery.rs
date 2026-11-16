use super::support::{notifies, receipt, refer, status, Fixture};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt};
use rusterisk_sip::{Datagram, ReferFailure, ReferProgress, SipError, SipEvent};
use std::net::SocketAddr;

fn changed(packet: &Datagram, peer: SocketAddr, bytes: Vec<u8>) -> Datagram {
    let mut changed = packet.clone();
    changed.peer = peer;
    changed.bytes = bytes;
    changed
}

#[test]
fn delivery_identity_matches_only_its_peer_dialog_and_refer_sequence() -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let identity = fixture
        .engine
        .refer_identity(requested.key())
        .ok_or(SipError::InvalidPacket)?;
    assert_eq!(identity.peer(), fixture.peer);
    let local_tag = format!(
        "tag={}",
        fixture
            .answer
            .to_header()?
            .typed()?
            .tag()
            .ok_or(SipError::InvalidPacket)?
            .value()
    );
    let accepted = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
    assert_eq!(accepted.datagrams.len(), 2);
    for packet in &accepted.datagrams {
        assert!(identity.matches(packet)?);
        let text = std::str::from_utf8(&packet.bytes)?;
        for (from, to) in [
            ("Call-ID: refer-dialog", "Call-ID: unrelated"),
            ("tag=caller", "tag=unrelated"),
            (local_tag.as_str(), "tag=unrelated"),
            ("refer;id=3", "refer;id=4"),
            ("CSeq: 3 REFER", "CSeq: 4 REFER"),
        ] {
            if !text.contains(from) {
                continue;
            }
            assert!(!identity.matches(&changed(
                packet,
                packet.peer,
                text.replace(from, to).into_bytes()
            ))?);
        }
        assert!(!identity.matches(&changed(
            packet,
            "127.0.0.1:5062".parse()?,
            packet.bytes.clone()
        ))?);
        let mut oversized = packet.bytes.clone();
        oversized.resize(rusterisk_sip::MAX_MESSAGE + 1, 0);
        assert!(matches!(
            identity.matches(&changed(packet, packet.peer, oversized)),
            Err(SipError::Capacity)
        ));
        for duplicate in ["From", "To", "Call-ID", "CSeq"] {
            let header = text
                .lines()
                .find(|line| line.starts_with(&format!("{duplicate}:")))
                .ok_or(SipError::InvalidPacket)?;
            assert!(matches!(
                identity.matches(&changed(
                    packet,
                    packet.peer,
                    text.replacen("\r\n\r\n", &format!("\r\n{header}\r\n\r\n"), 1)
                        .into_bytes()
                )),
                Err(SipError::InvalidPacket)
            ));
        }
    }
    let bye = fixture.engine.cancel(&fixture.id, 7, 7)?;
    for packet in &bye.datagrams {
        assert!(!identity.matches(packet)?);
    }
    Ok(())
}

#[test]
fn delivery_identity_survives_final_owner_removal_for_the_final_response() -> Result<(), SipError> {
    for suppressed in [false, true] {
        let mut fixture = Fixture::new(true, true)?;
        let requested = refer(fixture.receive(
            "REFER",
            3,
            if suppressed {
                "Refer-To: <sip:200@target.invalid>\r\nRefer-Sub: false\r\n"
            } else {
                "Refer-To: <sip:200@target.invalid>\r\n"
            },
            5,
        )?)?;
        let identity = fixture
            .engine
            .refer_identity(requested.key())
            .ok_or(SipError::InvalidPacket)?;
        let rejected = fixture.engine.refer_progress(
            requested.key(),
            ReferProgress::Rejected(ReferFailure::Unavailable),
            6,
        )?;
        assert!(fixture.engine.refer_identity(requested.key()).is_none());
        assert_eq!(rejected.datagrams.len(), 1);
        assert!(identity.matches(rejected.datagrams.first().ok_or(SipError::InvalidPacket)?)?);
        assert!(rejected.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == requested.key())));
    }
    Ok(())
}

#[test]
fn closing_wire_ownership_cancels_notify_retries_and_preserves_the_invite_usage(
) -> Result<(), SipError> {
    let mut fixture = Fixture::new(true, true)?;
    let requested =
        refer(fixture.receive("REFER", 3, "Refer-To: <sip:200@target.invalid>\r\n", 5)?)?;
    let accepted = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Trying, 6)?;
    assert_eq!(notifies(&accepted)?.len(), 1);
    let closed = fixture.engine.cancel_refer(requested.key());
    assert!(closed.datagrams.is_empty());
    assert_eq!(closed.events.len(), 1);
    assert!(closed.events.iter().any(|event| matches!(event, SipEvent::ReferSubscriptionEnded { refer } if *refer == requested.key())));
    assert!(fixture
        .engine
        .cancel_refer(requested.key())
        .events
        .is_empty());
    assert!(fixture.engine.refer_identity(requested.key()).is_none());
    let retries = fixture.engine.tick(1_000)?;
    assert!(notifies(&retries)?.is_empty());
    assert!(status(&retries, 401));
    let digit = rusterisk_core::DtmfDigit::from_event(1).ok_or(SipError::InvalidPacket)?;
    let sent = fixture
        .engine
        .send_info(&fixture.id, 7, digit, 100, 1_001)?;
    let info =
        super::support::request(&sent.datagrams.first().ok_or(SipError::InvalidPacket)?.bytes)?;
    fixture
        .engine
        .receive(&receipt(&info, 200)?, fixture.peer, 1_002)?;
    let late = fixture
        .engine
        .refer_progress(requested.key(), ReferProgress::Answered, 1_003)?;
    assert!(late.datagrams.is_empty());
    assert!(late.events.is_empty());
    Ok(())
}
