use rusterisk_core::{DtmfDigit, DtmfTone};
use rusterisk_media::{AudioFormat, MediaError, RtpSender, TelephoneEvent, TelephoneSender};

fn sender() -> Result<(RtpSender, TelephoneSender), MediaError> {
    let audio = RtpSender::new(
        AudioFormat::Opus,
        111,
        [0, 0, 0, 7, 255, 254, 255, 255, 255, 250],
    )?;
    let events = TelephoneSender::new(&audio, 102, 48000, [0; 10])?;
    Ok((audio, events))
}

fn long_tone() -> DtmfTone {
    DtmfTone {
        digit: DtmfDigit::D,
        duration_ms: 5000,
    }
}

#[test]
fn every_long_event_segment_repeats_its_final_report_at_the_update_interval(
) -> Result<(), MediaError> {
    let (mut audio, mut sender) = sender()?;
    let timestamp = sender.timestamp_at(0, &audio);
    sender.start(long_tone(), 0, &audio)?;
    let mut endings = [0; 4];
    let mut reported = [[0; 3]; 4];
    let mut sequence = 65534_u16;
    let mut first = true;
    for now in (20..=5040).step_by(20) {
        let packets = sender.poll(now, &mut audio)?;
        assert!(packets.len() <= 5);
        for packet in packets {
            assert_eq!(packet.sequence(), sequence);
            sequence = sequence.wrapping_add(1);
            assert_eq!(packet.ssrc(), 7);
            assert_eq!(packet.payload_type(), 102);
            assert_eq!(packet.marker(), first);
            first = false;
            let offset = packet.timestamp().wrapping_sub(timestamp);
            assert_eq!(offset % u32::from(u16::MAX), 0);
            let segment = (offset / u32::from(u16::MAX)) as usize;
            assert!(segment < 4);
            let event = TelephoneEvent::parse(packet.payload())?;
            assert_eq!(event.digit(), DtmfDigit::D);
            assert_eq!(event.ended(), segment == 3 && now >= 5000);
            if event.duration() == u16::MAX || event.ended() {
                assert!(endings[segment] < 3);
                reported[segment][endings[segment]] = now;
                endings[segment] += 1;
            }
        }
    }
    assert_eq!(endings, [3; 4]);
    for reports in reported {
        assert_eq!(reports[1] - reports[0], 20);
        assert_eq!(reports[2] - reports[1], 20);
    }
    assert!(sender.ready());
    assert!(sender.poll(5060, &mut audio)?.is_empty());
    Ok(())
}

#[test]
fn delayed_poll_reports_every_segment_without_restarting_the_long_event() -> Result<(), MediaError>
{
    let (mut audio, mut sender) = sender()?;
    let timestamp = sender.timestamp_at(0, &audio);
    sender.start(long_tone(), 0, &audio)?;
    let mut sequence = 65534_u16;
    for (copy, now) in [5000, 5020, 5040].into_iter().enumerate() {
        let packets = sender.poll(now, &mut audio)?;
        assert_eq!(packets.len(), 4);
        for (segment, packet) in packets.into_iter().enumerate() {
            assert_eq!(packet.sequence(), sequence);
            sequence = sequence.wrapping_add(1);
            assert_eq!(
                packet.timestamp(),
                timestamp.wrapping_add(segment as u32 * u32::from(u16::MAX))
            );
            assert_eq!(packet.marker(), copy == 0 && segment == 0);
            let event = TelephoneEvent::parse(packet.payload())?;
            assert_eq!(event.digit(), DtmfDigit::D);
            assert_eq!(event.ended(), segment == 3);
            assert_eq!(
                event.duration(),
                if segment == 3 { 43395 } else { u16::MAX }
            );
        }
    }
    assert!(sender.ready());
    assert!(sender.poll(5060, &mut audio)?.is_empty());
    Ok(())
}

#[test]
fn delayed_segments_fit_beside_an_earlier_events_pending_repetitions() -> Result<(), MediaError> {
    let (mut audio, mut sender) = sender()?;
    sender.start(
        DtmfTone {
            digit: DtmfDigit::One,
            duration_ms: 1,
        },
        0,
        &audio,
    )?;
    assert_eq!(sender.poll(1, &mut audio)?.len(), 1);
    sender.start(long_tone(), 1, &audio)?;
    let mut endings = [0; 4];
    let timestamp = sender.timestamp_at(1, &audio);
    for (now, expected) in [(5001, 5), (5021, 5), (5041, 4)] {
        let packets = sender.poll(now, &mut audio)?;
        assert_eq!(packets.len(), expected);
        for packet in packets {
            let event = TelephoneEvent::parse(packet.payload())?;
            if event.digit() == DtmfDigit::D {
                let offset = packet.timestamp().wrapping_sub(timestamp);
                assert_eq!(offset % u32::from(u16::MAX), 0);
                let segment = (offset / u32::from(u16::MAX)) as usize;
                assert!(segment < 4);
                endings[segment] += 1;
            } else {
                assert_eq!(event.digit(), DtmfDigit::One);
                assert!(event.ended());
            }
        }
    }
    assert_eq!(endings, [3; 4]);
    assert!(sender.ready());
    Ok(())
}

#[test]
fn millisecond_durations_report_partial_segments_at_each_clock_boundary() -> Result<(), MediaError>
{
    let cases: &[(u32, u32, &[u16])] = &[
        (48000, 1365, &[65520]),
        (48000, 1366, &[65535, 33]),
        (48000, 2730, &[65535, 65505]),
        (48000, 2731, &[65535, 65535, 18]),
        (48000, 4095, &[65535, 65535, 65490]),
        (48000, 4096, &[65535, 65535, 65535, 3]),
        (8000, 5000, &[40000]),
    ];
    for &(rate, duration_ms, expected) in cases {
        let mut audio = RtpSender::new(AudioFormat::Opus, 111, [0; 10])?;
        let mut sender = TelephoneSender::new(&audio, 102, rate, [1; 10])?;
        sender.start(
            DtmfTone {
                digit: DtmfDigit::D,
                duration_ms,
            },
            0,
            &audio,
        )?;
        for copy in 0..3 {
            let packets = sender.poll(u64::from(duration_ms) + copy * 20, &mut audio)?;
            assert_eq!(packets.len(), expected.len());
            for (index, (packet, duration)) in packets.iter().zip(expected).enumerate() {
                let event = TelephoneEvent::parse(packet.payload())?;
                assert_eq!(event.duration(), *duration);
                assert_eq!(event.ended(), index + 1 == expected.len());
                assert_eq!(packet.marker(), copy == 0 && index == 0);
            }
        }
        assert!(sender.ready());
    }
    Ok(())
}

#[test]
fn cancellation_discards_active_segments_and_all_queued_repetitions() -> Result<(), MediaError> {
    for now in [1400, 5000] {
        let (mut audio, mut sender) = sender()?;
        sender.start(long_tone(), 0, &audio)?;
        assert!(!sender.poll(now, &mut audio)?.is_empty());
        sender.cancel();
        assert!(sender.ready());
        assert!(sender.poll(now + 20, &mut audio)?.is_empty());
        sender.start(
            DtmfTone {
                digit: DtmfDigit::Hash,
                duration_ms: 20,
            },
            now + 20,
            &audio,
        )?;
        let packets = sender.poll(now + 40, &mut audio)?;
        assert_eq!(packets.len(), 1);
        assert!(packets[0].marker());
        let event = TelephoneEvent::parse(packets[0].payload())?;
        assert_eq!(event.digit(), DtmfDigit::Hash);
        assert_eq!(event.duration(), 960);
        assert!(event.ended());
    }
    Ok(())
}
