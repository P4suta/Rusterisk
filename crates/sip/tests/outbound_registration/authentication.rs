use super::{
    responses::{final_response, response},
    settings, transaction, transmitted,
};
use rusterisk_config::{Auth, Config, Protocol, RegistrarUri};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationResponse, RegistrationTransaction, SipError,
};
use sha2::{Digest, Sha256, Sha512_256};

pub(super) fn credentials() -> Result<Vec<Auth>, SipError> {
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        "[wildcard]\ntype=auth\nusername=account\npassword=wrong\nrealm=*\n[exact]\ntype=auth\nusername=account\npassword=sample\nrealm=provider.example\n[proxy]\ntype=auth\nusername=proxy\npassword=sample\nrealm=proxy.example\n",
        "",
        "",
    )?.auth)
}

fn challenge(offer: &str) -> Result<RegistrationResponse, SipError> {
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    final_response(pending.receive(&response(&packet, 401, offer)?, packet.peer, 100)?)
}

pub(super) fn header<'a>(packet: &'a Datagram, name: &str) -> Result<&'a str, SipError> {
    std::str::from_utf8(&packet.bytes)?
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
        .ok_or(SipError::InvalidPacket)
}

pub(super) fn hash<D: Digest>(source: &str) -> String {
    D::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn sha512_integrity_uses_the_actual_register_uri_and_empty_entity_hash() -> Result<(), SipError> {
    let mut settings = settings("udp", "")?;
    let uri = "SIP:Provider.Example:05060;transport=UDP;extension=%61b";
    settings.server_uri =
        RegistrarUri::parse(Protocol::Udp, uri).map_err(|_| SipError::InvalidPacket)?;
    let identity = RegistrationIdentity::new(0x1234, 7, 0x5678)?;
    let mut pending = RegistrationTransaction::new(
        &settings,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        identity,
        0,
    )?;
    let packet = transmitted(pending.poll(0)?)?;
    let offer = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-512-256, qop=\"auth,auth-int\"\r\n";
    let receipt =
        final_response(pending.receive(&response(&packet, 401, offer)?, packet.peer, 100)?)?;
    let mut retry = receipt
        .authenticated_retry(&credentials()?, identity.advance(9)?, "client-one", 1, 200)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(retry.poll(200)?)?;
    let wire = std::str::from_utf8(&packet.bytes)?;
    assert!(wire.starts_with(&format!("REGISTER {uri} SIP/2.0\r\n")));
    assert!(wire.contains("CSeq: 8 REGISTER\r\n"));
    let authorization = header(&packet, "Authorization")?;
    assert!(authorization.contains("username=\"account\""));
    assert!(authorization.contains(&format!("uri=\"{uri}\"")));
    assert!(authorization.contains("algorithm=SHA-512-256, qop=auth-int"));
    let identity_hash = hash::<Sha512_256>("account:provider.example:sample");
    let entity = hash::<Sha512_256>("");
    let target = hash::<Sha512_256>(&format!("REGISTER:{uri}:{entity}"));
    let expected = hash::<Sha512_256>(&format!(
        "{identity_hash}:server-nonce:00000001:client-one:auth-int:{target}"
    ));
    assert!(authorization.contains(&format!("response=\"{expected}\"")));
    assert!(wire.contains("Content-Length: 0\r\n"));
    assert_eq!(wire.split_once("\r\n\r\n").map(|(_, body)| body), Some(""));
    Ok(())
}

#[test]
fn server_priority_and_default_quality_use_the_shared_challenge_contract() -> Result<(), SipError> {
    let offer = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-256\r\nWWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=MD5\r\n";
    let receipt = challenge(offer)?;
    let identity = receipt.identity().advance(9)?;
    let uri = receipt.request_uri()?.to_owned();
    let mut retry = receipt
        .authenticated_retry(&credentials()?, identity, "client-one", 9, 200)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(retry.poll(200)?)?;
    let authorization = header(&packet, "Authorization")?;
    assert!(authorization.contains("algorithm=SHA-256, qop=auth"));
    assert!(authorization.contains("nc=00000009"));
    let a1 = hash::<Sha256>("account:provider.example:sample");
    let a2 = hash::<Sha256>(&format!("REGISTER:{uri}"));
    let expected = hash::<Sha256>(&format!("{a1}:server-nonce:00000009:client-one:auth:{a2}"));
    assert!(authorization.contains(&format!("response=\"{expected}\"")));
    Ok(())
}

#[test]
fn proxy_challenges_keep_the_existing_origin_authorization() -> Result<(), SipError> {
    let records = credentials()?;
    let receipt = challenge("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-256, qop=\"auth\"\r\n")?;
    let identity = receipt.identity().advance(9)?;
    let mut retry = receipt
        .authenticated_retry(&records, identity, "client-one", 1, 200)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(retry.poll(200)?)?;
    let previous = header(&packet, "Authorization")?.to_owned();
    let offer = "Proxy-Authenticate: Digest realm=\"proxy.example\", nonce=\"proxy-nonce\", algorithm=SHA-256, qop=\"auth\"\r\n";
    let receipt =
        final_response(retry.receive(&response(&packet, 407, offer)?, packet.peer, 300)?)?;
    let mut next = receipt
        .authenticated_retry(&records, identity.advance(10)?, "client-two", 1, 400)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(next.poll(400)?)?;
    let origin = header(&packet, "Authorization")?;
    assert_ne!(origin, previous);
    assert!(origin.contains("nonce=\"server-nonce\""));
    assert!(origin.contains("nc=00000002"));
    let a1 = hash::<Sha256>("account:provider.example:sample");
    let a2 = hash::<Sha256>("REGISTER:SIP:Provider.Example:05060;transport=UDP");
    let expected = hash::<Sha256>(&format!("{a1}:server-nonce:00000002:client-one:auth:{a2}"));
    assert!(origin.contains(&format!("response=\"{expected}\"")));
    let proxy = header(&packet, "Proxy-Authorization")?;
    assert!(proxy.contains("username=\"proxy\""));
    assert!(proxy.contains("nonce=\"proxy-nonce\""));
    assert!(std::str::from_utf8(&packet.bytes)?.contains("CSeq: 9 REGISTER\r\n"));
    Ok(())
}

#[test]
fn malformed_unsupported_or_unmatched_offers_never_fabricate_authentication() -> Result<(), SipError>
{
    for offer in [
        "Basic realm=\"provider.example\"",
        "Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-3",
        "Digest realm=\"provider.example\", nonce=\"server-nonce\", qop=\"extension\"",
        "Digest realm=\"provider.example\", nonce=\"server-nonce\", charset=ISO-8859-1",
        "Digest realm=\"provider.example\", nonce=\"server-nonce\", userhash=true",
    ] {
        let receipt = challenge(&format!("WWW-Authenticate: {offer}\r\n"))?;
        let identity = receipt.identity().advance(9)?;
        assert!(receipt
            .authenticated_retry(&credentials()?, identity, "client-one", 1, 200)?
            .is_none());
    }
    let mut records = credentials()?;
    records.retain(|record| record.realm != "*");
    for record in &mut records {
        record.realm = "another.example".into();
    }
    let receipt = challenge(
        "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\"\r\n",
    )?;
    let identity = receipt.identity().advance(9)?;
    assert!(receipt
        .authenticated_retry(&records, identity, "client-one", 1, 200)?
        .is_none());
    Ok(())
}

#[test]
fn retry_arguments_require_a_fresh_sequence_and_valid_nonce_counter_and_clock(
) -> Result<(), SipError> {
    let records = credentials()?;
    let offer = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-256\r\n";
    for identity in [
        RegistrationIdentity::new(0x1234, 7, 9)?,
        RegistrationIdentity::new(0x7777, 8, 9)?,
    ] {
        assert!(matches!(
            challenge(offer)?.authenticated_retry(&records, identity, "client-one", 1, 200),
            Err(SipError::InvalidPacket)
        ));
    }
    for (client, counter, now) in [
        ("", 1, 200),
        ("client\r\nheader", 1, 200),
        ("ä", 1, 200),
        ("client-one", 0, 200),
        ("client-one", 1, 99),
    ] {
        let receipt = challenge(offer)?;
        let identity = receipt.identity().advance(9)?;
        assert!(matches!(
            receipt.authenticated_retry(&records, identity, client, counter, now),
            Err(SipError::InvalidPacket)
        ));
    }
    Ok(())
}

#[test]
fn stale_retries_require_a_changed_nonce_and_have_a_finite_authentication_budget(
) -> Result<(), SipError> {
    let records = credentials()?;
    for (nonce, stale, accepted) in [
        ("server-nonce", true, false),
        ("changed", false, false),
        ("changed", true, true),
    ] {
        let receipt = challenge("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"server-nonce\", algorithm=SHA-256\r\n")?;
        let identity = receipt.identity().advance(9)?;
        let mut retry = receipt
            .authenticated_retry(&records, identity, "client-one", 1, 200)?
            .ok_or(SipError::Unsupported)?;
        let packet = transmitted(retry.poll(200)?)?;
        let offer = format!("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"{nonce}\", algorithm=SHA-256, stale={stale}\r\n");
        let receipt =
            final_response(retry.receive(&response(&packet, 401, &offer)?, packet.peer, 300)?)?;
        assert_eq!(
            receipt
                .authenticated_retry(&records, identity.advance(10)?, "client-two", 1, 400)?
                .is_some(),
            accepted
        );
    }
    let mut pending = transaction("udp", 0)?;
    for attempt in 0..5_u32 {
        let now = u64::from(attempt) * 200;
        let packet = transmitted(pending.poll(now)?)?;
        let offer = format!("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"nonce-{attempt}\", algorithm=SHA-256, stale=true\r\n");
        let receipt = final_response(pending.receive(
            &response(&packet, 401, &offer)?,
            packet.peer,
            now + 100,
        )?)?;
        let identity = receipt.identity().advance(u128::from(attempt) + 20)?;
        let next = receipt.authenticated_retry(&records, identity, "client-one", 1, now + 200)?;
        if attempt == 4 {
            assert!(next.is_none());
            break;
        }
        pending = next.ok_or(SipError::Unsupported)?;
    }
    Ok(())
}
