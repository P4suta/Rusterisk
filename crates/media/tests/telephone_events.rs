use rusterisk_core::DtmfDigit;
use rusterisk_media::{MediaError, RtpPacket, TelephoneEvent, TelephoneReceiver};

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

fn packet(
    digit: DtmfDigit,
    timestamp: u32,
    duration: u16,
    ended: bool,
    sequence: u16,
) -> Result<RtpPacket, MediaError> {
    RtpPacket::new(
        101,
        sequence,
        timestamp,
        7,
        &TelephoneEvent::new(digit, ended, 10, duration)?.encode(),
    )
}

#[test]
fn repeated_end_reports_emit_one_digit_and_missing_ends_expire_without_duplicates(
) -> Result<(), MediaError> {
    let mut receiver = TelephoneReceiver::new(101, 8000)?;
    assert_eq!(
        receiver
            .push(&packet(DtmfDigit::Hash, 1000, 160, false, 1)?, 0)?
            .into_tones()
            .count(),
        0
    );
    let completed: Vec<_> = receiver
        .push(&packet(DtmfDigit::Hash, 1000, 800, true, 2)?, 80)?
        .into_tones()
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].digit, DtmfDigit::Hash);
    assert_eq!(completed[0].duration_ms, 100);
    for sequence in [2, 3, 4] {
        assert_eq!(
            receiver
                .push(&packet(DtmfDigit::Hash, 1000, 800, true, sequence)?, 100)?
                .into_tones()
                .count(),
            0
        );
    }
    receiver.push(&packet(DtmfDigit::Five, 2600, 480, false, 5)?, 150)?;
    assert!(receiver.expire(299)?.is_none());
    let expired = receiver.expire(300)?.ok_or(MediaError::InvalidPacket)?;
    assert_eq!(expired.digit, DtmfDigit::Five);
    assert_eq!(expired.duration_ms, 60);
    assert!(receiver.expire(1000)?.is_none());
    assert_eq!(
        receiver
            .push(&packet(DtmfDigit::Five, 2600, 800, true, 6)?, 1100)?
            .into_tones()
            .count(),
        0
    );
    assert!(receiver
        .push(&RtpPacket::new(101, 7, 3600, 8, &[5, 0x80, 0, 160])?, 1200)
        .is_err());
    Ok(())
}

#[test]
fn long_events_span_timestamp_wrap_and_late_segments_do_not_create_extra_digits(
) -> Result<(), MediaError> {
    let mut receiver = TelephoneReceiver::new(101, 8000)?;
    let start = u32::MAX - 40000;
    receiver.push(&packet(DtmfDigit::One, start, u16::MAX, false, 65535)?, 0)?;
    let end = packet(
        DtmfDigit::One,
        start.wrapping_add(u32::from(u16::MAX)),
        800,
        true,
        0,
    )?;
    let tones: Vec<_> = receiver.push(&end, 20)?.into_tones().collect();
    assert_eq!(tones.len(), 1);
    assert_eq!(tones[0].duration_ms, 8291);
    assert_eq!(
        receiver
            .push(&packet(DtmfDigit::One, start, u16::MAX, false, 65535)?, 21)?
            .into_tones()
            .count(),
        0
    );
    assert!(receiver.expire(1000)?.is_none());
    Ok(())
}

#[test]
fn a_new_digit_closes_an_incomplete_event_and_invalid_packets_claim_no_source(
) -> Result<(), MediaError> {
    let mut receiver = TelephoneReceiver::new(101, 8000)?;
    assert!(receiver
        .push(&RtpPacket::new(101, 1, 0, 8, &[16, 0x80, 0, 160])?, 0)
        .is_err());
    receiver.push(&packet(DtmfDigit::A, 0, 160, false, 1)?, 0)?;
    let tones: Vec<_> = receiver
        .push(&packet(DtmfDigit::B, 800, 320, true, 2)?, 40)?
        .into_tones()
        .collect();
    assert_eq!(tones.len(), 2);
    assert_eq!(tones[0].digit, DtmfDigit::A);
    assert_eq!(tones[0].duration_ms, 20);
    assert_eq!(tones[1].digit, DtmfDigit::B);
    assert_eq!(tones[1].duration_ms, 40);
    assert!(receiver.expire(1000)?.is_none());
    Ok(())
}
