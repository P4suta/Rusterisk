use super::{authorization, engine, offer, packet, response, Name};
use rusterisk_sip::SipError;

#[derive(Clone, Copy)]
enum Mismatch {
    Realm,
    PeerAddress,
    PeerPort,
    CallId,
}

#[test]
fn forged_nonce_contexts_reject_registration_without_consuming_the_outstanding_nonce(
) -> Result<(), SipError> {
    for mismatch in [
        Mismatch::Realm,
        Mismatch::PeerAddress,
        Mismatch::PeerPort,
        Mismatch::CallId,
    ] {
        let mut engine = engine("", "SHA-256")?;
        let peer = "127.0.0.1:5098".parse()?;
        let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
        let offered = offer(&challenge, "SHA-256")?;
        let altered = match mismatch {
            Mismatch::Realm => {
                offered.replace("realm=\"example.net\"", "realm=\"another.example\"")
            }
            Mismatch::PeerAddress | Mismatch::PeerPort | Mismatch::CallId => offered.clone(),
        };
        let signed = authorization(&altered, "alice", "sample", Name::Hashed)?;
        let wire = match mismatch {
            Mismatch::CallId => packet(2, &signed)
                .replace("Call-ID: inbound-digest\r\n", "Call-ID: another-call\r\n"),
            Mismatch::Realm | Mismatch::PeerAddress | Mismatch::PeerPort => packet(2, &signed),
        };
        let sender = match mismatch {
            Mismatch::PeerAddress => "127.0.0.2:5098".parse()?,
            Mismatch::PeerPort => "127.0.0.1:5099".parse()?,
            Mismatch::Realm | Mismatch::CallId => peer,
        };
        let rejected = engine.receive(wire.as_bytes(), sender, 1)?;
        assert_eq!(response(&rejected)?.status_code.code(), 401);
        assert!(rejected.events.is_empty());
        assert_eq!(engine.registered_peer("phone", 1), None);
        let signed = authorization(&offered, "alice", "sample", Name::Hashed)?;
        let accepted = engine.receive(packet(3, &signed).as_bytes(), peer, 2)?;
        assert_eq!(response(&accepted)?.status_code.code(), 200);
        assert_eq!(engine.registered_peer("phone", 2), Some(peer));
    }
    Ok(())
}

#[test]
fn nonce_expiration_ends_at_the_exact_sixty_second_boundary() -> Result<(), SipError> {
    for (now, expected) in [(59_999, 200), (60_000, 401)] {
        let mut engine = engine("", "SHA-256")?;
        let peer = "127.0.0.1:5098".parse()?;
        let challenge = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
        let signed = authorization(
            &offer(&challenge, "SHA-256")?,
            "alice",
            "sample",
            Name::Plain,
        )?;
        let output = engine.receive(packet(2, &signed).as_bytes(), peer, now)?;
        assert_eq!(response(&output)?.status_code.code(), expected);
        if expected == 401 {
            assert!(output.events.is_empty());
            assert_eq!(engine.registered_peer("phone", now), None);
        } else {
            assert_eq!(engine.registered_peer("phone", now), Some(peer));
        }
    }
    Ok(())
}

#[test]
fn rejecting_an_expired_nonce_preserves_another_unexpired_nonce() -> Result<(), SipError> {
    let mut engine = engine("", "SHA-256")?;
    let peer = "127.0.0.1:5098".parse()?;
    let old = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    let fresh = response(&engine.receive(packet(2, "").as_bytes(), peer, 1000)?)?;
    let old = authorization(&offer(&old, "SHA-256")?, "alice", "sample", Name::Plain)?;
    let fresh = authorization(&offer(&fresh, "SHA-256")?, "alice", "sample", Name::Plain)?;
    let rejected = engine.receive(packet(3, &old).as_bytes(), peer, 60_000)?;
    assert_eq!(response(&rejected)?.status_code.code(), 401);
    assert!(rejected.events.is_empty());
    assert_eq!(engine.registered_peer("phone", 60_000), None);
    let accepted = engine.receive(packet(4, &fresh).as_bytes(), peer, 60_001)?;
    assert_eq!(response(&accepted)?.status_code.code(), 200);
    assert_eq!(engine.registered_peer("phone", 60_001), Some(peer));
    Ok(())
}
