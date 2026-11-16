use super::{
    authentication::{credentials, hash, header},
    responses::{final_response, response},
    transaction, transmitted,
};
use rusterisk_sip::{Datagram, RegistrationResponse, RegistrationTransaction, SipError};
use sha2::Sha256;
use std::num::NonZeroU32;

fn receipt(
    pending: &mut RegistrationTransaction,
    now: u64,
    count: u32,
    status: u16,
) -> Result<RegistrationResponse, SipError> {
    let packet = transmitted(pending.poll(now)?)?;
    check(&packet, count)?;
    let fields = format!("Contact: {};expires=120\r\n", header(&packet, "Contact")?);
    final_response(pending.receive(&response(&packet, status, &fields)?, packet.peer, now + 100)?)
}

fn check(packet: &Datagram, count: u32) -> Result<(), SipError> {
    let authorization = header(packet, "Authorization")?;
    let username = hash::<Sha256>("account:provider.example");
    assert!(authorization.contains(&format!("username=\"{username}\"")));
    assert!(authorization.contains("userhash=true"));
    assert!(authorization.contains(&format!("nc={count:08x}")));
    let uri = std::str::from_utf8(&packet.bytes)?
        .split_once(" SIP/2.0")
        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
        .ok_or(SipError::InvalidPacket)?;
    let identity = hash::<Sha256>("account:provider.example:sample");
    let target = hash::<Sha256>(&format!("REGISTER:{uri}"));
    let expected = hash::<Sha256>(&format!(
        "{identity}:hashed-nonce:{count:08x}:hashed-client:auth:{target}"
    ));
    assert!(authorization.contains(&format!("response=\"{expected}\"")));
    Ok(())
}

#[test]
fn failed_retry_refresh_and_removal_preserve_the_hashed_identity() -> Result<(), SipError> {
    let records = credentials()?;
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let offer = "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"hashed-nonce\", algorithm=SHA-256, qop=\"auth\", charset=UTF-8, userhash=true\r\n";
    let receipt =
        final_response(pending.receive(&response(&packet, 401, offer)?, packet.peer, 100)?)?;
    let identity = receipt.identity().advance(10)?;
    let mut pending = receipt
        .authenticated_retry(&records, identity, "hashed-client", 1, 200)?
        .ok_or(SipError::Unsupported)?;
    let failed = self::receipt(&mut pending, 200, 1, 500)?;
    let mut retried = failed.retry_after_failure(&records, failed.identity().advance(11)?, 400)?;
    let granted = self::receipt(&mut retried, 400, 2, 200)?;
    let mut refreshed = granted.refresh(
        &records,
        granted.identity().advance(12)?,
        NonZeroU32::new(120).ok_or(SipError::InvalidPacket)?,
        600,
    )?;
    let granted = self::receipt(&mut refreshed, 600, 3, 200)?;
    let mut removed = granted.remove(&records, granted.identity().advance(13)?, 800)?;
    let packet = transmitted(removed.poll(800)?)?;
    check(&packet, 4)?;
    assert_eq!(header(&packet, "Expires")?, "0");
    Ok(())
}
