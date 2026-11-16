use super::{
    authentication::header,
    responses::{final_response, response},
    settings, transaction, transmitted,
};
use rusterisk_config::{Protocol, RegistrarUri};
use rusterisk_sip::{
    Datagram, RegistrationIdentity, RegistrationResponse, RegistrationTransaction, SipError,
};
#[path = "alternate/peer_authentication.rs"]
mod peer_authentication;

pub(super) fn received(
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

pub(super) fn same_identity(original: &Datagram, next: &Datagram) -> Result<(), SipError> {
    for name in [
        "From",
        "To",
        "Call-ID",
        "Contact",
        "Expires",
        "Content-Length",
    ] {
        assert_eq!(header(next, name)?, header(original, name)?);
    }
    let original = std::str::from_utf8(&original.bytes)?;
    let next = std::str::from_utf8(&next.bytes)?;
    assert_eq!(original.lines().next(), next.lines().next());
    assert_eq!(
        original.split_once("\r\n\r\n").map(|(_, body)| body),
        next.split_once("\r\n\r\n").map(|(_, body)| body)
    );
    Ok(())
}

#[test]
fn alternate_peer_preserves_wire_identity_body_and_retransmission() -> Result<(), SipError> {
    let uri = "SIP:Provider.Example:05060;transport=UDP;extension=%61b";
    let mut settings = settings("udp", "")?;
    settings.server_uri =
        RegistrarUri::parse(Protocol::Udp, uri).map_err(|_| SipError::InvalidPacket)?;
    let pending = RegistrationTransaction::new(
        &settings,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        0,
    )?;
    let (receipt, original) = received(pending, 503, "Retry-After: 10\r\n", 0)?;
    let identity = receipt.identity().advance(0x5678)?;
    let backup = "127.0.0.3:5070".parse()?;
    let mut next = receipt.retry_alternate(&[], identity, backup, 200)?;
    assert_eq!(next.expires_at(), 32_200);
    assert!(matches!(next.poll(199), Err(SipError::InvalidPacket)));
    let packet = transmitted(next.poll(200)?)?;
    assert_eq!(packet.peer, backup);
    same_identity(&original, &packet)?;
    assert_ne!(header(&packet, "Via")?, header(&original, "Via")?);
    assert_eq!(header(&packet, "CSeq")?, "8 REGISTER");
    assert_eq!(receipt.peer(), original.peer);
    assert_eq!(receipt.request_uri()?, uri);
    assert_eq!(
        receipt.retry_after()?.map(|interval| interval.get()),
        Some(10)
    );
    assert!(next.poll(699)?.is_none());
    let retry = transmitted(next.poll(700)?)?;
    assert_eq!(retry.peer, packet.peer);
    assert_eq!(retry.bytes, packet.bytes);
    next.cancel();
    assert!(next.poll(u64::MAX)?.is_none());
    Ok(())
}

#[test]
fn alternate_builder_requires_a_service_unavailable_udp_response() -> Result<(), SipError> {
    let backup = "127.0.0.3:5070".parse()?;
    for status in [200, 301, 401, 407, 408, 423, 500, 504, 603] {
        let (receipt, _) = received(transaction("udp", 0)?, status, "", 0)?;
        assert!(matches!(
            receipt.retry_alternate(&[], receipt.identity().advance(9)?, backup, 200),
            Err(SipError::Unsupported)
        ));
    }
    for protocol in ["tcp", "tls"] {
        let (receipt, _) = received(transaction(protocol, 0)?, 503, "", 0)?;
        assert!(matches!(
            receipt.retry_alternate(&[], receipt.identity().advance(9)?, backup, 200),
            Err(SipError::Unsupported)
        ));
    }
    for fields in [
        "",
        "Retry-After: 0\r\n",
        "Retry-After: 10 (overload);duration=5\r\n",
    ] {
        let (receipt, _) = received(transaction("udp", 0)?, 503, fields, 0)?;
        receipt.retry_alternate(&[], receipt.identity().advance(9)?, backup, 200)?;
    }
    Ok(())
}

#[test]
fn alternate_builder_rejects_invalid_routes_identity_and_clock() -> Result<(), SipError> {
    let (receipt, _) = received(transaction("udp", 0)?, 503, "Retry-After: 10\r\n", 0)?;
    for peer in [
        "127.0.0.2:5070",
        "0.0.0.0:5070",
        "224.0.0.1:5070",
        "127.0.0.3:0",
        "[::1]:5070",
    ] {
        assert!(matches!(
            receipt.retry_alternate(&[], receipt.identity().advance(9)?, peer.parse()?, 200),
            Err(SipError::InvalidPacket)
        ));
    }
    let backup = "127.0.0.3:5070".parse()?;
    for identity in [
        RegistrationIdentity::new(0x1234, 7, 9)?,
        RegistrationIdentity::new(0x1234, 6, 9)?,
        RegistrationIdentity::new(0x9999, 8, 9)?,
    ] {
        assert!(matches!(
            receipt.retry_alternate(&[], identity, backup, 200),
            Err(SipError::InvalidPacket)
        ));
    }
    assert!(matches!(
        receipt.retry_alternate(&[], receipt.identity().advance(9)?, backup, 99),
        Err(SipError::InvalidPacket)
    ));
    assert!(matches!(
        receipt.retry_alternate(&[], receipt.identity().advance(9)?, backup, u64::MAX),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn alternate_builder_validates_the_retry_instruction_without_consuming_the_response(
) -> Result<(), SipError> {
    for fields in [
        "Retry-After: 10\r\nRetry-After: 20\r\n",
        "Retry-After: 10 (unfinished\r\n",
        "Retry-After: 10;duration=\"5\"\r\n",
    ] {
        let (receipt, _) = received(transaction("udp", 0)?, 503, fields, 0)?;
        let identity = receipt.identity();
        assert!(matches!(
            receipt.retry_alternate(&[], identity.advance(9)?, "127.0.0.3:5070".parse()?, 200),
            Err(SipError::InvalidPacket)
        ));
        assert_eq!(receipt.identity(), identity);
        assert_eq!(receipt.status(), 503);
    }
    Ok(())
}

#[test]
fn negative_retry_instructions_are_rejected_before_a_registration_receipt() -> Result<(), SipError>
{
    let mut pending = transaction("udp", 0)?;
    let packet = transmitted(pending.poll(0)?)?;
    let malformed = response(&packet, 503, "Retry-After: -1\r\n")?;
    assert!(matches!(
        pending.receive(&malformed, packet.peer, 100),
        Err(SipError::Parse(rsipstack::sip::Error::ParseError(reason)))
            if reason == "Retry-After: invalid value"
    ));
    assert_eq!(pending.next_deadline(), Some(500));
    let valid = response(&packet, 503, "Retry-After: 10\r\n")?;
    let receipt = final_response(pending.receive(&valid, packet.peer, 101)?)?;
    assert_eq!(receipt.identity(), pending.identity());
    assert_eq!(receipt.peer(), packet.peer);
    assert_eq!(receipt.retry_after()?.map(|value| value.get()), Some(10));
    receipt.retry_alternate(
        &[],
        receipt.identity().advance(9)?,
        "127.0.0.3:5070".parse()?,
        200,
    )?;
    Ok(())
}
