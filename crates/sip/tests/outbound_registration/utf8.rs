use super::{
    authentication::{hash, header},
    responses::{final_response, response},
    transaction, transmitted,
};
use rusterisk_config::{Auth, Config};
use rusterisk_sip::{Datagram, RegistrationResponse, RegistrationTransaction, SipError};
use sha2::Sha256;
use std::num::NonZeroU32;

fn credentials() -> Result<Vec<Auth>, SipError> {
    Ok(Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        "[origin]\ntype=auth\nusername=account\npassword=pa\u{030a}ss\nrealm=provider.example\n[proxy]\ntype=auth\nusername=proxy\npassword=pa\u{030a}ss\nrealm=proxy.example\n",
        "",
        "",
    )?.auth)
}

fn received(
    pending: &mut RegistrationTransaction,
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

fn authenticated() -> Result<RegistrationTransaction, SipError> {
    let mut pending = transaction("udp", 0)?;
    let (receipt, _) = received(
        &mut pending,
        401,
        "WWW-Authenticate: Digest realm=\"provider.example\", nonce=\"utf8-nonce\", algorithm=SHA-256, qop=\"auth\", charset=UTF-8\r\n",
        0,
    )?;
    let identity = receipt.identity().advance(10)?;
    receipt
        .authenticated_retry(&credentials()?, identity, "utf8-client", 1, 200)?
        .ok_or(SipError::Unsupported)
}

fn expected(
    username: &str,
    realm: &str,
    password: &str,
    nonce: &str,
    client: &str,
    count: u32,
    uri: &str,
) -> String {
    let identity = hash::<Sha256>(&format!("{username}:{realm}:{password}"));
    let target = hash::<Sha256>(&format!("REGISTER:{uri}"));
    hash::<Sha256>(&format!(
        "{identity}:{nonce}:{count:08x}:{client}:auth:{target}"
    ))
}

#[test]
fn successful_refresh_keeps_the_negotiated_encoding_without_a_wire_hint() -> Result<(), SipError> {
    let mut pending = authenticated()?;
    let packet = transmitted(pending.poll(200)?)?;
    let uri = std::str::from_utf8(&packet.bytes)?
        .split_once(" SIP/2.0")
        .and_then(|(line, _)| line.strip_prefix("REGISTER "))
        .ok_or(SipError::InvalidPacket)?;
    assert!(!header(&packet, "Authorization")?.contains("charset="));
    let fields = format!("Contact: {};expires=120\r\n", header(&packet, "Contact")?);
    let receipt =
        final_response(pending.receive(&response(&packet, 200, &fields)?, packet.peer, 300)?)?;
    let mut refresh = receipt.refresh(
        &credentials()?,
        receipt.identity().advance(11)?,
        NonZeroU32::new(120).ok_or(SipError::InvalidPacket)?,
        400,
    )?;
    let refreshed = transmitted(refresh.poll(400)?)?;
    let expected = expected(
        "account",
        "provider.example",
        "p\u{00e5}ss",
        "utf8-nonce",
        "utf8-client",
        2,
        uri,
    );
    assert!(header(&refreshed, "Authorization")?.contains(&format!("response=\"{expected}\"")));
    Ok(())
}

#[test]
fn proxy_authentication_keeps_each_scope_encoding_independent() -> Result<(), SipError> {
    let mut pending = authenticated()?;
    let (receipt, _) = received(
        &mut pending,
        407,
        "Proxy-Authenticate: Digest realm=\"proxy.example\", nonce=\"raw-nonce\", algorithm=SHA-256, qop=\"auth\"\r\n",
        200,
    )?;
    let uri = receipt.request_uri()?.to_owned();
    let identity = receipt.identity().advance(12)?;
    let mut pending = receipt
        .authenticated_retry(&credentials()?, identity, "raw-client", 1, 400)?
        .ok_or(SipError::Unsupported)?;
    let packet = transmitted(pending.poll(400)?)?;
    let origin = expected(
        "account",
        "provider.example",
        "p\u{00e5}ss",
        "utf8-nonce",
        "utf8-client",
        2,
        &uri,
    );
    let proxy = expected(
        "proxy",
        "proxy.example",
        "pa\u{030a}ss",
        "raw-nonce",
        "raw-client",
        1,
        &uri,
    );
    assert!(header(&packet, "Authorization")?.contains(&format!("response=\"{origin}\"")));
    assert!(header(&packet, "Proxy-Authorization")?.contains(&format!("response=\"{proxy}\"")));
    Ok(())
}
