use super::{
    authentication::{credentials, hash, header},
    responses::{final_response, response},
    settings, transaction, transmitted,
};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationResponse, RegistrationTransaction, SipError,
};
use sha2::{Sha256, Sha512_256};

fn declined(
    mut pending: RegistrationTransaction,
    status: u16,
    headers: &str,
    now: u64,
) -> Result<(RegistrationResponse, Datagram), SipError> {
    let packet = transmitted(pending.poll(now)?)?;
    let receipt = final_response(pending.receive(
        &response(&packet, status, headers)?,
        packet.peer,
        now + 100,
    )?)?;
    Ok((receipt, packet))
}

#[test]
fn corrections_preserve_uri_roles_and_raise_the_transmitted_expiration() -> Result<(), SipError> {
    for protocol in ["udp", "tcp", "tls"] {
        let (receipt, original) =
            declined(transaction(protocol, 0)?, 423, "Min-Expires: 007200\r\n", 0)?;
        assert_eq!(
            receipt.minimum_expiration()?.map(|value| value.get()),
            Some(7200)
        );
        let identity = receipt.identity().advance(99)?;
        let mut retry = receipt
            .minimum_expiration_retry(&[], identity, 200)?
            .ok_or(SipError::Unsupported)?;
        assert_eq!(retry.expires_at(), 32_200);
        assert_eq!(retry.next_deadline(), Some(200));
        let packet = transmitted(retry.poll(200)?)?;
        assert_eq!(packet.peer, original.peer);
        for name in ["From", "To", "Call-ID", "Contact", "Content-Length"] {
            assert_eq!(header(&packet, name)?, header(&original, name)?);
        }
        assert_eq!(
            std::str::from_utf8(&packet.bytes)?
                .split_once("\r\n")
                .map(|(line, _)| line),
            std::str::from_utf8(&original.bytes)?
                .split_once("\r\n")
                .map(|(line, _)| line)
        );
        assert_eq!(header(&packet, "Expires")?, "7200");
        assert_eq!(header(&packet, "CSeq")?, "8 REGISTER");
        assert_ne!(header(&packet, "Via")?, header(&original, "Via")?);
    }
    Ok(())
}

#[test]
fn malformed_minimum_values_cannot_generate_a_correction() -> Result<(), SipError> {
    for headers in [
        "",
        "Min-Expires: 0\r\n",
        "Min-Expires: 3599\r\n",
        "Min-Expires: 3600\r\n",
        "Min-Expires: -1\r\n",
        "Min-Expires: +7200\r\n",
        "Min-Expires: 7200x\r\n",
        "Min-Expires: 4294967296\r\n",
        "Min-Expires: 7200\r\nMin-Expires: 7200\r\n",
        "Min-Expires: 7200,7201\r\n",
    ] {
        let (receipt, _) = declined(transaction("udp", 0)?, 423, headers, 0)?;
        let identity = receipt.identity().advance(99)?;
        assert!(matches!(
            receipt.minimum_expiration(),
            Err(SipError::InvalidPacket)
        ));
        assert!(matches!(
            receipt.minimum_expiration_retry(&[], identity, 200),
            Err(SipError::InvalidPacket)
        ));
    }
    Ok(())
}

#[test]
fn removal_and_unrelated_responses_never_become_registration_corrections() -> Result<(), SipError> {
    for status in [200, 401, 407, 403, 500, 503] {
        let (receipt, _) = declined(transaction("udp", 0)?, status, "Min-Expires: 7200\r\n", 0)?;
        let identity = receipt.identity().advance(99)?;
        assert!(receipt.minimum_expiration()?.is_none());
        assert!(receipt
            .minimum_expiration_retry(&[], identity, 200)?
            .is_none());
    }
    let pending = RegistrationTransaction::new(
        &settings("udp", "expiration=0\n")?,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        0,
    )?;
    let (receipt, _) = declined(pending, 423, "Min-Expires: 7200\r\n", 0)?;
    let identity = receipt.identity().advance(99)?;
    assert!(receipt.minimum_expiration()?.is_none());
    assert!(receipt
        .minimum_expiration_retry(&[], identity, 200)?
        .is_none());
    Ok(())
}

#[test]
fn correction_checks_identity_clock_and_deadline_before_emitting_bytes() -> Result<(), SipError> {
    for (identity, now) in [
        (RegistrationIdentity::new(0x1234, 7, 99)?, 200),
        (RegistrationIdentity::new(0x9999, 8, 99)?, 200),
        (RegistrationIdentity::new(0x1234, 8, 99)?, 99),
        (RegistrationIdentity::new(0x1234, 8, 99)?, u64::MAX),
    ] {
        let (receipt, _) = declined(transaction("udp", 0)?, 423, "Min-Expires: 7200\r\n", 0)?;
        assert!(receipt
            .minimum_expiration_retry(&[], identity, now)
            .is_err());
    }
    let (receipt, _) = declined(
        transaction("udp", 0)?,
        423,
        "Min-Expires: 4294967295\r\n",
        0,
    )?;
    let identity = receipt.identity().advance(99)?;
    let mut pending = receipt
        .minimum_expiration_retry(&[], identity, 200)?
        .ok_or(SipError::Unsupported)?;
    assert_eq!(
        header(&transmitted(pending.poll(200)?)?, "Expires")?,
        "4294967295"
    );
    pending.cancel();
    assert!(pending.poll(u64::MAX)?.is_none());
    Ok(())
}

const ORIGIN: &str = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"origin-nonce\", algorithm=SHA-512-256, qop=\"auth-int\", opaque=\"origin-opaque\"\r\n";
const PROXY: &str = "Proxy-Authenticate: Digest realm=\"proxy.example\", nonce=\"proxy-nonce\", algorithm=SHA-256, qop=\"auth\", opaque=\"proxy-opaque\"\r\n";

fn authenticated(scopes: &[(u16, &str)], count: u32) -> Result<RegistrationTransaction, SipError> {
    let mut pending = transaction("udp", 0)?;
    let mut now = 0;
    for (status, offer) in scopes {
        let (receipt, _) = declined(pending, *status, offer, now)?;
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
    Ok(pending)
}

#[test]
fn corrections_renew_both_authentication_scopes_with_the_wire_uri() -> Result<(), SipError> {
    let (receipt, original) = declined(
        authenticated(&[(401, ORIGIN), (407, PROXY)], 1)?,
        423,
        "Min-Expires: 7200\r\n",
        400,
    )?;
    let identity = receipt.identity().advance(99)?;
    let uri = receipt.request_uri()?.to_owned();
    let mut pending = receipt
        .minimum_expiration_retry(&credentials()?, identity, 600)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(pending.poll(600)?)?;
    assert_eq!(header(&packet, "Contact")?, header(&original, "Contact")?);
    assert_eq!(uri, "SIP:Provider.Example:05060;transport=UDP");
    let origin = header(&packet, "Authorization")?;
    let a1 = hash::<Sha512_256>("account:provider.example:sample");
    let body = hash::<Sha512_256>("");
    let a2 = hash::<Sha512_256>(&format!("REGISTER:{uri}:{body}"));
    let expected = hash::<Sha512_256>(&format!(
        "{a1}:origin-nonce:00000003:client-401:auth-int:{a2}"
    ));
    assert!(origin.contains(&format!("response=\"{expected}\"")));
    assert!(origin.contains("opaque=\"origin-opaque\""));
    let proxy = header(&packet, "Proxy-Authorization")?;
    let a1 = hash::<Sha256>("proxy:proxy.example:sample");
    let a2 = hash::<Sha256>(&format!("REGISTER:{uri}"));
    let expected = hash::<Sha256>(&format!("{a1}:proxy-nonce:00000002:client-407:auth:{a2}"));
    assert!(proxy.contains(&format!("response=\"{expected}\"")));
    assert!(proxy.contains("opaque=\"proxy-opaque\""));
    Ok(())
}

#[test]
fn missing_credentials_and_nonce_exhaustion_cannot_generate_corrections() -> Result<(), SipError> {
    for scope in [[(401, ORIGIN)], [(407, PROXY)]] {
        for (records, count) in [(Vec::new(), 1), (credentials()?, u32::MAX)] {
            let (receipt, _) = declined(
                authenticated(&scope, count)?,
                423,
                "Min-Expires: 7200\r\n",
                400,
            )?;
            let identity = receipt.identity().advance(99)?;
            assert!(receipt
                .minimum_expiration_retry(&records, identity, 600)
                .is_err());
        }
    }
    Ok(())
}

#[test]
fn repeated_corrections_do_not_reset_the_authentication_attempt_budget() -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    for attempt in 0..5_u32 {
        let now = u64::from(attempt) * 400;
        let offer = format!("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"nonce-{attempt}\", algorithm=SHA-256, stale=true\r\n");
        let (receipt, _) = declined(pending, 401, &offer, now)?;
        let identity = receipt.identity().advance(u128::from(attempt))?;
        let retry =
            receipt.authenticated_retry(&credentials()?, identity, "client-one", 1, now + 200)?;
        if attempt == 4 {
            assert!(retry.is_none());
            break;
        }
        let (receipt, _) = declined(
            retry.ok_or(SipError::Unsupported)?,
            423,
            &format!("Min-Expires: {}\r\n", 3601 + attempt),
            now + 200,
        )?;
        let identity = receipt.identity().advance(99)?;
        pending = receipt
            .minimum_expiration_retry(&credentials()?, identity, now + 400)?
            .ok_or(SipError::Unsupported)?;
    }
    Ok(())
}
