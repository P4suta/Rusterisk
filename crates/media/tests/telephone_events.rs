use rusterisk_core::{DtmfDigit, DtmfTone};
use rusterisk_media::{
    AudioFormat, Frame, MediaError, RtpPacket, RtpSender, TelephoneEvent, TelephoneFormat,
    TelephoneReceiver, TelephoneSender,
};

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

#[test]
fn telephone_transmission_shares_matching_clocks_and_repeats_the_final_report(
) -> Result<(), MediaError> {
    let mut audio = RtpSender::new(
        AudioFormat::Pcmu,
        0,
        [0, 0, 0, 7, 255, 254, 255, 255, 255, 155],
    )?;
    let mut events = TelephoneSender::new(&audio, 101, 8000, [0; 10])?;
    let first = audio.packet(Frame::new(&[0; 160], 8000)?)?;
    assert_eq!(first.sequence(), 65534);
    events.start(
        DtmfTone {
            digit: DtmfDigit::Hash,
            duration_ms: 60,
        },
        0,
        &audio,
    )?;
    assert!(events
        .start(
            DtmfTone {
                digit: DtmfDigit::One,
                duration_ms: 20
            },
            0,
            &audio
        )
        .is_err());
    assert!(events.poll(19, &mut audio)?.is_empty());
    let packets = events.poll(20, &mut audio)?;
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].sequence(), 65535);
    assert_eq!(packets[0].ssrc(), 7);
    assert_eq!(packets[0].timestamp(), first.timestamp());
    assert!(packets[0].marker());
    assert_eq!(TelephoneEvent::parse(packets[0].payload())?.duration(), 160);
    assert_eq!(audio.packet(Frame::new(&[0; 160], 8000)?)?.sequence(), 0);
    assert_eq!(events.poll(40, &mut audio)?[0].sequence(), 1);
    let final_packet = events.poll(60, &mut audio)?;
    assert!(events.ready());
    for (index, now) in [60, 80, 100].into_iter().enumerate() {
        let copies = if index == 0 {
            final_packet.clone()
        } else {
            events.poll(now, &mut audio)?
        };
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].sequence(), 2 + index as u16);
        assert_eq!(copies[0].timestamp(), first.timestamp());
        assert!(!copies[0].marker());
        let payload = TelephoneEvent::parse(copies[0].payload())?;
        assert!(payload.ended());
        assert_eq!(payload.duration(), 480);
    }
    assert!(events.poll(120, &mut audio)?.is_empty());
    events.start(
        DtmfTone {
            digit: DtmfDigit::One,
            duration_ms: 20,
        },
        120,
        &audio,
    )?;
    events.cancel();
    assert!(events.poll(1000, &mut audio)?.is_empty());
    assert!(events
        .start(
            DtmfTone {
                digit: DtmfDigit::One,
                duration_ms: 0
            },
            1000,
            &audio
        )
        .is_err());
    Ok(())
}

#[test]
fn different_event_clocks_use_distinct_sources_and_long_tones_span_segments(
) -> Result<(), MediaError> {
    let mut audio = RtpSender::new(AudioFormat::Opus, 111, [0; 10])?;
    let mut separate = TelephoneSender::new(&audio, 101, 8000, [0; 10])?;
    separate.start(
        DtmfTone {
            digit: DtmfDigit::A,
            duration_ms: 20,
        },
        100,
        &audio,
    )?;
    let packets = separate.poll(120, &mut audio)?;
    assert_ne!(packets[0].ssrc(), 0);
    assert_eq!(packets[0].timestamp(), 800);
    assert_eq!(TelephoneEvent::parse(packets[0].payload())?.duration(), 160);
    assert_eq!(audio.packet(Frame::new(&[0; 960], 48000)?)?.sequence(), 0);

    let mut events = TelephoneSender::new(&audio, 102, 48000, [0; 10])?;
    let mut receiver = TelephoneReceiver::new(102, 48000)?;
    events.start(
        DtmfTone {
            digit: DtmfDigit::D,
            duration_ms: 5000,
        },
        0,
        &audio,
    )?;
    let mut received = Vec::new();
    for now in [20, 1400, 2800, 4200, 5000, 5020, 5040] {
        let packets = events.poll(now, &mut audio)?;
        assert!(packets.len() <= 4);
        for packet in packets {
            received.extend(receiver.push(&packet, now)?.into_tones());
        }
    }
    assert_eq!(
        received,
        [DtmfTone {
            digit: DtmfDigit::D,
            duration_ms: 5000
        }]
    );
    Ok(())
}

#[test]
fn short_tones_cannot_overfill_the_terminal_repetition_queue() -> Result<(), MediaError> {
    let mut audio = RtpSender::new(AudioFormat::Pcmu, 0, [0; 10])?;
    let mut events = TelephoneSender::new(&audio, 101, 8000, [1; 10])?;
    let tone = DtmfTone {
        digit: DtmfDigit::One,
        duration_ms: 1,
    };
    events.start(tone, 0, &audio)?;
    assert_eq!(events.poll(1, &mut audio)?.len(), 1);
    events.start(tone, 1, &audio)?;
    assert_eq!(events.poll(2, &mut audio)?.len(), 1);
    assert!(!events.ready());
    assert!(events.start(tone, 2, &audio).is_err());
    assert_eq!(events.poll(22, &mut audio)?.len(), 2);
    assert_eq!(events.poll(42, &mut audio)?.len(), 2);
    assert!(events.ready());
    events.start(tone, 42, &audio)?;
    assert_eq!(events.poll(43, &mut audio)?.len(), 1);
    Ok(())
}

#[test]
fn repeated_digits_have_distinct_start_times_while_audio_is_silent() -> Result<(), MediaError> {
    let mut audio = RtpSender::new(AudioFormat::Pcmu, 0, [0; 10])?;
    let mut events = TelephoneSender::new(&audio, 101, 8000, [1; 10])?;
    let mut receiver = TelephoneReceiver::new(101, 8000)?;
    let tone = DtmfTone {
        digit: DtmfDigit::One,
        duration_ms: 20,
    };
    let mut received = Vec::new();
    for start in [0, 100] {
        events.start(tone, start, &audio)?;
        for now in [start + 20, start + 40, start + 60] {
            for packet in events.poll(now, &mut audio)? {
                assert_eq!(packet.timestamp(), start as u32 * 8);
                received.extend(receiver.push(&packet, now)?.into_tones());
            }
        }
    }
    assert_eq!(received, [tone, tone]);
    Ok(())
}

#[test]
fn senders_refuse_digits_outside_the_negotiated_event_set() -> Result<(), MediaError> {
    let audio = RtpSender::new(AudioFormat::Pcmu, 0, [0; 10])?;
    let format = TelephoneFormat::new(101, 8000)?.with_events(1 << DtmfDigit::Hash.event())?;
    let mut sender = TelephoneSender::with_format(&audio, format, [1; 10])?;
    assert!(sender
        .start(
            DtmfTone {
                digit: DtmfDigit::One,
                duration_ms: 100
            },
            0,
            &audio
        )
        .is_err());
    assert!(sender.ready());
    sender.start(
        DtmfTone {
            digit: DtmfDigit::Hash,
            duration_ms: 100,
        },
        0,
        &audio,
    )?;
    assert!(TelephoneFormat::new(101, 8000)?.with_events(0).is_err());
    Ok(())
}
