use super::{
    authentication::{credentials, hash, header},
    responses::{final_response, response},
    transaction, transmitted,
};
use rusterisk_sip::{
    Datagram, RegistrationGrant, RegistrationIdentity, RegistrationResponse,
    RegistrationTransaction, SipError,
};
use sha2::{Sha256, Sha512_256};
use std::num::NonZeroU32;

fn duration(seconds: u32) -> Result<NonZeroU32, SipError> {
    NonZeroU32::new(seconds).ok_or(SipError::InvalidPacket)
}

fn registered(
    mut pending: RegistrationTransaction,
    sent: u64,
    received: u64,
) -> Result<(RegistrationResponse, Datagram), SipError> {
    let packet = transmitted(pending.poll(sent)?)?;
    let headers = format!("Contact: {};expires=3600\r\n", header(&packet, "Contact")?);
    let receipt = final_response(pending.receive(
        &response(&packet, 200, &headers)?,
        packet.peer,
        received,
    )?)?;
    Ok((receipt, packet))
}

fn authenticated(
    scopes: &[(u16, &str)],
    count: u32,
) -> Result<(RegistrationResponse, Datagram), SipError> {
    let mut pending = transaction("udp", 0)?;
    let mut now = 0;
    for (status, offer) in scopes {
        let packet = transmitted(pending.poll(now)?)?;
        let receipt = final_response(pending.receive(
            &response(&packet, *status, offer)?,
            packet.peer,
            now + 100,
        )?)?;
        let identity = receipt.identity().advance(u128::from(*status))?;
        now += 200;
        pending = receipt
            .authenticated_retry(
                &credentials()?,
                identity,
                &format!("client-{status}"),
                count,
                now,
            )?
            .ok_or(SipError::Unsupported)?;
    }
    registered(pending, now, now + 100)
}

const ORIGIN: &str = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"origin-nonce\", algorithm=SHA-512-256, qop=\"auth-int\", opaque=\"origin-opaque\"\r\n";
const PROXY: &str = "Proxy-Authenticate: Digest realm=\"proxy.example\", nonce=\"proxy-nonce\", algorithm=SHA-256, qop=\"auth\", opaque=\"proxy-opaque\"\r\n";

#[test]
fn refresh_preserves_wire_identity_route_and_contact_across_native_transports(
) -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let (receipt, original) = registered(transaction(protocol, 0)?, 0, 100)?;
        let identity = receipt.identity().advance(99)?;
        let mut pending = receipt.refresh(&[], identity, duration(90)?, 200)?;
        assert_eq!(pending.identity(), identity);
        assert_eq!(pending.expires_at(), 32_200);
        assert_eq!(pending.next_deadline(), Some(200));
        let packet = transmitted(pending.poll(200)?)?;
        assert_eq!(packet.peer, original.peer);
        for field in ["From", "To", "Call-ID", "Contact", "Content-Length"] {
            assert_eq!(header(&packet, field)?, header(&original, field)?);
        }
        let wire = std::str::from_utf8(&packet.bytes)?;
        assert_eq!(
            wire.split_once("\r\n").map(|(line, _)| line),
            std::str::from_utf8(&original.bytes)?
                .split_once("\r\n")
                .map(|(line, _)| line)
        );
        assert!(header(&packet, "Via")?
            .contains("branch=z9hG4bK-reg-00000000000000000000000000000063-8"));
        assert_eq!(header(&packet, "CSeq")?, "8 REGISTER");
        assert_eq!(header(&packet, "Expires")?, "90");
        assert!(wire.ends_with("\r\n\r\n"));
        assert_eq!(
            receipt.request_uri()?,
            wire.split(' ').nth(1).ok_or(SipError::InvalidPacket)?
        );
    }
    Ok(())
}

#[test]
fn removal_preserves_the_owned_contact_and_requires_an_active_grant() -> Result<(), SipError> {
    let (receipt, original) = registered(transaction("udp", 0)?, 0, 100)?;
    let mut pending = receipt.remove(&[], receipt.identity().advance(99)?, 200)?;
    let packet = transmitted(pending.poll(200)?)?;
    assert_eq!(header(&packet, "Contact")?, header(&original, "Contact")?);
    assert_eq!(header(&packet, "Expires")?, "0");
    let removed =
        final_response(pending.receive(&response(&packet, 200, "")?, packet.peer, 300)?)?;
    assert_eq!(removed.grant()?, RegistrationGrant::Removed);
    let identity = removed.identity().advance(100)?;
    assert!(removed.remove(&[], identity, 400).is_err());
    assert!(removed.refresh(&[], identity, duration(90)?, 400).is_err());
    Ok(())
}

#[test]
fn refresh_and_removal_renew_each_authentication_scope_with_the_actual_wire_uri(
) -> Result<(), SipError> {
    for remove in [false, true] {
        let (receipt, original) = authenticated(&[(401, ORIGIN), (407, PROXY)], 1)?;
        let identity = receipt.identity().advance(99)?;
        let records = credentials()?;
        let mut pending = if remove {
            receipt.remove(&records, identity, 1000)?
        } else {
            receipt.refresh(&records, identity, duration(90)?, 1000)?
        };
        let packet = transmitted(pending.poll(1000)?)?;
        let uri = receipt.request_uri()?;
        assert_eq!(uri, "SIP:Provider.Example:05060;transport=UDP");
        assert_eq!(header(&packet, "Contact")?, header(&original, "Contact")?);
        let origin = header(&packet, "Authorization")?;
        assert!(origin.contains("nc=00000003"));
        assert!(origin.contains("cnonce=\"client-401\""));
        assert!(origin.contains("opaque=\"origin-opaque\""));
        assert!(origin.contains(&format!("uri=\"{uri}\"")));
        let a1 = hash::<Sha512_256>("account:provider.example:sample");
        let body = hash::<Sha512_256>("");
        let a2 = hash::<Sha512_256>(&format!("REGISTER:{uri}:{body}"));
        let expected = hash::<Sha512_256>(&format!(
            "{a1}:origin-nonce:00000003:client-401:auth-int:{a2}"
        ));
        assert!(origin.contains(&format!("response=\"{expected}\"")));
        let proxy = header(&packet, "Proxy-Authorization")?;
        assert!(proxy.contains("nc=00000002"));
        assert!(proxy.contains("cnonce=\"client-407\""));
        assert!(proxy.contains("opaque=\"proxy-opaque\""));
        let a1 = hash::<Sha256>("proxy:proxy.example:sample");
        let a2 = hash::<Sha256>(&format!("REGISTER:{uri}"));
        let expected = hash::<Sha256>(&format!("{a1}:proxy-nonce:00000002:client-407:auth:{a2}"));
        assert!(proxy.contains(&format!("response=\"{expected}\"")));
    }
    Ok(())
}

#[test]
fn missing_credentials_and_exhausted_nonce_counts_cannot_emit_updates() -> Result<(), SipError> {
    for scope in [[(401, ORIGIN)], [(407, PROXY)]] {
        let (receipt, _) = authenticated(&scope, 1)?;
        let identity = receipt.identity().advance(99)?;
        assert!(receipt.refresh(&[], identity, duration(90)?, 1000).is_err());
        assert!(receipt.remove(&[], identity, 1000).is_err());
        let (receipt, _) = authenticated(&scope, u32::MAX)?;
        let identity = receipt.identity().advance(99)?;
        assert!(matches!(
            receipt.refresh(&credentials()?, identity, duration(90)?, 1000),
            Err(SipError::Capacity)
        ));
        assert!(matches!(
            receipt.remove(&credentials()?, identity, 1000),
            Err(SipError::Capacity)
        ));
    }
    Ok(())
}

#[test]
fn update_identity_clock_and_deadline_checks_precede_transmission() -> Result<(), SipError> {
    let (receipt, _) = registered(transaction("udp", 0)?, 0, 100)?;
    for identity in [
        RegistrationIdentity::new(0x1234, 7, 99)?,
        RegistrationIdentity::new(0x9999, 8, 99)?,
    ] {
        assert!(receipt.refresh(&[], identity, duration(90)?, 200).is_err());
        assert!(receipt.remove(&[], identity, 200).is_err());
    }
    let identity = receipt.identity().advance(99)?;
    assert!(receipt.refresh(&[], identity, duration(90)?, 99).is_err());
    assert!(receipt.remove(&[], identity, 99).is_err());
    assert!(matches!(
        receipt.refresh(&[], identity, duration(90)?, u64::MAX),
        Err(SipError::Capacity)
    ));
    assert!(matches!(
        receipt.remove(&[], identity, u64::MAX),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn failed_or_ambiguous_grants_cannot_generate_a_refresh_or_removal() -> Result<(), SipError> {
    for (status, headers) in [
        (200, ""),
        (401, ORIGIN),
        (
            500,
            "Contact: <sip:line+1@127.0.0.1:5068;transport=udp>;expires=90\r\n",
        ),
    ] {
        let mut pending = transaction("udp", 0)?;
        let packet = transmitted(pending.poll(0)?)?;
        let receipt = final_response(pending.receive(
            &response(&packet, status, headers)?,
            packet.peer,
            100,
        )?)?;
        let identity = receipt.identity().advance(99)?;
        assert!(receipt.refresh(&[], identity, duration(90)?, 200).is_err());
        assert!(receipt.remove(&[], identity, 200).is_err());
    }
    Ok(())
}

#[test]
fn refreshed_transactions_have_a_fresh_finite_authentication_budget() -> Result<(), SipError> {
    let (receipt, _) = authenticated(&[(401, ORIGIN), (407, PROXY)], 1)?;
    let records = credentials()?;
    let mut pending = receipt.refresh(
        &records,
        receipt.identity().advance(99)?,
        duration(90)?,
        1000,
    )?;
    for attempt in 0..5_u32 {
        let now = 1000 + u64::from(attempt) * 200;
        let packet = transmitted(pending.poll(now)?)?;
        let offer = format!("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"refresh-{attempt}\", algorithm=SHA-256, stale=true\r\n");
        let receipt = final_response(pending.receive(
            &response(&packet, 401, &offer)?,
            packet.peer,
            now + 100,
        )?)?;
        let identity = receipt.identity().advance(u128::from(attempt) + 100)?;
        let retry =
            receipt.authenticated_retry(&records, identity, "refresh-client", 1, now + 200)?;
        if attempt == 4 {
            assert!(retry.is_none());
            break;
        }
        pending = retry.ok_or(SipError::Unsupported)?;
    }
    Ok(())
}
