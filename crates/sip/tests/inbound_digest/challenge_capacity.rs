use super::{attribute, authorization, configured, engine, offer, packet, response, Name};
use rusterisk_sip::{Engine, SipError};

fn profiles(total: u32) -> Result<Engine, SipError> {
    let extra: String = (1..total)
        .map(|index| format!("[zz-credential-{index:02}]\ntype=auth\nusername=alice\npassword=sample\nrealm=zone-{index:02}.example\nsupported_algorithms_uas=MD5\n[zz-phone-{index:02}]\ntype=endpoint\ntransport=udp\nauth=zz-credential-{index:02}\naors=line\n"))
        .collect();
    configured("", "MD5", "alice", "sample", &extra)
}

#[test]
fn nonce_capacity_rejects_new_challenges_without_invalidating_published_credentials(
) -> Result<(), SipError> {
    let mut engine = engine("", "MD5")?;
    let peer = "127.0.0.1:5098".parse()?;
    let first = response(&engine.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    let offered = offer(&first, "MD5")?;
    for sequence in 2..=128 {
        let output = engine.receive(packet(sequence, "").as_bytes(), peer, 0)?;
        assert_eq!(response(&output)?.status_code.code(), 401);
        assert!(output.events.is_empty());
    }
    let rejected = engine.receive(packet(129, "").as_bytes(), peer, 1)?;
    let rejected_response = response(&rejected)?;
    assert_eq!(rejected_response.status_code.code(), 503);
    assert!(!rejected_response
        .headers
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate")));
    assert!(rejected.events.is_empty());
    assert_eq!(engine.registered_peer("phone", 1), None);
    let signed = authorization(&offered, "alice", "sample", Name::Plain)?;
    let accepted = engine.receive(packet(130, &signed).as_bytes(), peer, 2)?;
    assert_eq!(response(&accepted)?.status_code.code(), 200);
    assert_eq!(engine.registered_peer("phone", 2), Some(peer));
    Ok(())
}

#[test]
fn an_unpublished_challenge_cannot_register_after_response_header_capacity_failure(
) -> Result<(), SipError> {
    let mut published = profiles(57)?;
    let mut unpublished = profiles(58)?;
    let peer = "127.0.0.1:5098".parse()?;
    let challenge = response(&published.receive(packet(1, "").as_bytes(), peer, 0)?)?;
    assert_eq!(challenge.status_code.code(), 401);
    assert_eq!(challenge.headers.iter().count(), 64);
    let offered = challenge
        .headers
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate"))
        .find(|header| attribute(header.value(), "realm").is_ok_and(|realm| realm == "example.net"))
        .ok_or(SipError::InvalidPacket)?
        .value();
    let denied = unpublished.receive(packet(1, "").as_bytes(), peer, 0)?;
    let denied_response = response(&denied)?;
    assert_eq!(denied_response.status_code.code(), 503);
    assert!(!denied_response
        .headers
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("WWW-Authenticate")));
    assert!(denied.events.is_empty());
    let signed = authorization(offered, "alice", "sample", Name::Plain)?;
    let accepted = published.receive(packet(2, &signed).as_bytes(), peer, 1)?;
    assert_eq!(response(&accepted)?.status_code.code(), 200);
    assert_eq!(published.registered_peer("phone", 1), Some(peer));
    let denied = unpublished.receive(packet(2, &signed).as_bytes(), peer, 1)?;
    assert_eq!(response(&denied)?.status_code.code(), 503);
    assert!(denied.events.is_empty());
    assert_eq!(unpublished.registered_peer("phone", 1), None);
    Ok(())
}
