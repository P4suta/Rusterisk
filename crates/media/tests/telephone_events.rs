use rusterisk_core::DtmfDigit;
use rusterisk_media::{MediaError, TelephoneEvent};

#[test]
fn telephone_event_payloads_match_wire_bytes_and_ignore_the_reserved_receive_bit(
) -> Result<(), MediaError> {
    let event = TelephoneEvent::new(DtmfDigit::Hash, true, 10, 960)?;
    assert_eq!(event.encode(), [11, 0x8a, 3, 0xc0]);
    let parsed = TelephoneEvent::parse(&[11, 0xca, 3, 0xc0])?;
    assert_eq!(parsed, event);
    assert_eq!(parsed.digit(), DtmfDigit::Hash);
    assert!(parsed.ended());
    assert_eq!(parsed.duration(), 960);
    assert!(TelephoneEvent::new(DtmfDigit::One, false, 64, 160).is_err());
    assert!(TelephoneEvent::parse(&[16, 0, 0, 160]).is_err());
    assert!(TelephoneEvent::parse(&[1, 0, 0]).is_err());
    assert!(TelephoneEvent::parse(&[1, 0, 0, 160, 0]).is_err());
    Ok(())
}
