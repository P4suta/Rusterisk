use super::{
    authentication::{credentials, hash, header},
    responses::{final_response, response},
    settings, transaction, transmitted,
};
use rusterisk_sip::{
    Datagram, RegistrationAuthentication, RegistrationIdentity, RegistrationResponse,
    RegistrationTransaction, SipError,
};
use sha2::{Sha256, Sha512_256};

fn received(
    mut pending: RegistrationTransaction,
    status: u16,
    fields: &str,
    now: u64,
) -> Result<(RegistrationResponse, Datagram), SipError> {
    let packet = transmitted(pending.poll(now)?)?;
    let receipt = final_response(pending.receive(
        &response(&packet, status, fields)?,
        packet.peer,
        now + 100,
    )?)?;
    Ok((receipt, packet))
}

fn retry(result: RegistrationAuthentication) -> Result<RegistrationTransaction, SipError> {
    match result {
        RegistrationAuthentication::Retry(transaction) => Ok(transaction),
        RegistrationAuthentication::Rejected(_response) => Err(SipError::Unsupported),
    }
}

const ORIGIN: &str = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"origin-nonce\", algorithm=SHA-512-256, qop=\"auth-int\"\r\n";
const PROXY: &str = "Proxy-Authenticate: Digest realm=\"proxy.example\", nonce=\"proxy-nonce\", algorithm=SHA-256, qop=\"auth\"\r\n";

#[test]
fn rejected_authentication_returns_the_owned_response_and_retry_instruction() -> Result<(), SipError>
{
    for (status, fields) in [
        (401, "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"nonce\", algorithm=SHA-3\r\nRetry-After: 9\r\n"),
        (500, "Retry-After: 9\r\n"),
    ] {
        let (receipt, packet) = received(transaction("udp", 0)?, status, fields, 0)?;
        let identity = receipt.identity();
        let uri = receipt.request_uri()?.to_owned();
        let RegistrationAuthentication::Rejected(receipt) = receipt.authenticate(&credentials()?, identity.advance(99)?, "client", 1, 200)? else {
            return Err(SipError::InvalidPacket);
        };
        assert_eq!(receipt.status(), status);
        assert_eq!(receipt.peer(), packet.peer);
        assert_eq!(receipt.identity(), identity);
        assert_eq!(receipt.request_uri()?, uri);
        assert_eq!(receipt.retry_after()?.map(|value| value.get()), Some(9));
    }
    Ok(())
}

#[test]
fn owned_authentication_keeps_the_existing_api_wire_contract() -> Result<(), SipError> {
    let (receipt, _) = received(transaction("udp", 0)?, 401, ORIGIN, 0)?;
    let identity = receipt.identity().advance(99)?;
    let mut owned = retry(receipt.authenticate(&credentials()?, identity, "client", 1, 200)?)?;
    let (receipt, _) = received(transaction("udp", 0)?, 401, ORIGIN, 0)?;
    let mut compatible = receipt
        .authenticated_retry(&credentials()?, identity, "client", 1, 200)?
        .ok_or(SipError::Unsupported)?;
    let owned = transmitted(owned.poll(200)?)?;
    let compatible = transmitted(compatible.poll(200)?)?;
    assert_eq!(owned.bytes, compatible.bytes);
    assert_eq!(owned.peer, compatible.peer);
    Ok(())
}

fn authenticated(scopes: &[(u16, &str)], count: u32) -> Result<RegistrationTransaction, SipError> {
    let mut pending = transaction("udp", 0)?;
    let mut now = 0;
    for (status, fields) in scopes {
        let (receipt, _) = received(pending, *status, fields, now)?;
        let identity = receipt.identity().advance(u128::from(*status))?;
        now += 200;
        pending = retry(receipt.authenticate(
            &credentials()?,
            identity,
            &format!("client-{status}"),
            count,
            now,
        )?)?;
    }
    Ok(pending)
}

#[test]
fn delayed_failure_retries_renew_each_scope_using_the_transmitted_uri() -> Result<(), SipError> {
    let pending = authenticated(&[(401, ORIGIN), (407, PROXY)], 1)?;
    let (receipt, original) = received(pending, 500, "Retry-After: 2\r\n", 400)?;
    let uri = receipt.request_uri()?.to_owned();
    let identity = receipt.identity().advance(99)?;
    let mut pending = receipt.retry_after_failure(&credentials()?, identity, 2500)?;
    assert_eq!(pending.expires_at(), 34_500);
    assert!(pending.poll(2499).is_err());
    let packet = transmitted(pending.poll(2500)?)?;
    for name in [
        "From",
        "To",
        "Call-ID",
        "Contact",
        "Expires",
        "Content-Length",
    ] {
        assert_eq!(header(&packet, name)?, header(&original, name)?);
    }
    assert_eq!(header(&packet, "CSeq")?, "10 REGISTER");
    let wire = std::str::from_utf8(&packet.bytes)?;
    assert!(wire.starts_with(&format!("REGISTER {uri} SIP/2.0\r\n")));
    let identity = hash::<Sha512_256>("account:provider.example:sample");
    let body = hash::<Sha512_256>("");
    let target = hash::<Sha512_256>(&format!("REGISTER:{uri}:{body}"));
    let expected = hash::<Sha512_256>(&format!(
        "{identity}:origin-nonce:00000003:client-401:auth-int:{target}"
    ));
    assert!(header(&packet, "Authorization")?.contains(&format!("response=\"{expected}\"")));
    let identity = hash::<Sha256>("proxy:proxy.example:sample");
    let target = hash::<Sha256>(&format!("REGISTER:{uri}"));
    let expected = hash::<Sha256>(&format!(
        "{identity}:proxy-nonce:00000002:client-407:auth:{target}"
    ));
    assert!(header(&packet, "Proxy-Authorization")?.contains(&format!("response=\"{expected}\"")));
    Ok(())
}

#[test]
fn failure_retries_preserve_removal_and_cannot_replace_success_or_minimum_correction(
) -> Result<(), SipError> {
    let pending = RegistrationTransaction::new(
        &settings("udp", "expiration=0\n")?,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        0,
    )?;
    let (receipt, _) = received(pending, 500, "", 0)?;
    let mut pending = receipt.retry_after_failure(&[], receipt.identity().advance(99)?, 200)?;
    assert_eq!(header(&transmitted(pending.poll(200)?)?, "Expires")?, "0");
    for status in [200, 423] {
        let (receipt, _) = received(transaction("udp", 0)?, status, "Min-Expires: 7200\r\n", 0)?;
        assert!(matches!(
            receipt.retry_after_failure(&[], receipt.identity().advance(99)?, 200),
            Err(SipError::Unsupported)
        ));
    }
    Ok(())
}

#[test]
fn failure_retry_checks_identity_clock_credentials_and_nonce_limits() -> Result<(), SipError> {
    let (receipt, _) = received(transaction("udp", 0)?, 500, "", 0)?;
    for (identity, now) in [
        (RegistrationIdentity::new(0x1234, 7, 99)?, 200),
        (RegistrationIdentity::new(0x9999, 8, 99)?, 200),
        (RegistrationIdentity::new(0x1234, 8, 99)?, 99),
        (RegistrationIdentity::new(0x1234, 8, 99)?, u64::MAX),
    ] {
        assert!(receipt.retry_after_failure(&[], identity, now).is_err());
    }
    for (records, count) in [(Vec::new(), 1), (credentials()?, u32::MAX)] {
        let (receipt, _) = received(authenticated(&[(401, ORIGIN)], count)?, 500, "", 200)?;
        assert!(receipt
            .retry_after_failure(&records, receipt.identity().advance(99)?, 400)
            .is_err());
    }
    Ok(())
}

#[test]
fn an_exhausted_authentication_cycle_returns_its_response_before_a_fresh_retry_cycle(
) -> Result<(), SipError> {
    let mut pending = transaction("udp", 0)?;
    let mut now = 0;
    for attempt in 0..5_u32 {
        let fields = format!("WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"cycle-{attempt}\", algorithm=SHA-256, stale=true\r\n");
        let (receipt, _) = received(pending, 401, &fields, now)?;
        let identity = receipt.identity().advance(u128::from(attempt))?;
        now += 200;
        match receipt.authenticate(&credentials()?, identity, "client", 1, now)? {
            RegistrationAuthentication::Retry(transaction) => pending = transaction,
            RegistrationAuthentication::Rejected(receipt) => {
                assert_eq!(attempt, 4);
                let identity = receipt.identity().advance(99)?;
                let pending = receipt.retry_after_failure(&credentials()?, identity, now + 2000)?;
                let (receipt, _) = received(pending, 401, "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"fresh\", algorithm=SHA-256, stale=true\r\n", now + 2000)?;
                let identity = receipt.identity().advance(100)?;
                let mut pending = retry(receipt.authenticate(
                    &credentials()?,
                    identity,
                    "fresh-client",
                    1,
                    now + 2200,
                )?)?;
                pending.cancel();
                assert!(pending.poll(u64::MAX)?.is_none());
                return Ok(());
            }
        }
    }
    Err(SipError::InvalidPacket)
}
