use rusterisk_media::{alaw_encode, mulaw_encode, AudioFormat, MediaError, RtpPacket, RtpReceiver};

fn codec_error(error: impl std::fmt::Display) -> MediaError {
    MediaError::Codec(error.to_string())
}

#[test]
fn opus_packet_durations_preserve_decoded_samples_in_complete_mixer_frames(
) -> Result<(), MediaError> {
    for samples in [120, 240, 480, 960, 1920, 2880, 5760] {
        let input =
            std::array::from_fn::<_, 5760, _>(|index| if index % 96 < 48 { 5000 } else { -5000 });
        let mut encoder = opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip)
            .map_err(codec_error)?;
        let mut encoded = [0; 1275];
        let mut combined = [0; 2048];
        let count = if samples < 960 { 960 / samples } else { 1 };
        let mut reference = opus::Decoder::new(48000, opus::Channels::Mono).map_err(codec_error)?;
        let mut expected = Vec::with_capacity(5760);
        let mut receiver = RtpReceiver::new(AudioFormat::Opus, 111)?;
        for index in 0..count {
            let size = encoder
                .encode(&input[..samples.min(2880)], &mut encoded)
                .map_err(codec_error)?;
            let packet = if samples == 5760 {
                let mut repacketizer = opus::Repacketizer::new().map_err(codec_error)?;
                let size = repacketizer
                    .combine(&[&encoded[..size], &encoded[..size]], &mut combined)
                    .map_err(codec_error)?;
                &combined[..size]
            } else {
                &encoded[..size]
            };
            let mut decoded = [0; 5760];
            let length = reference
                .decode(packet, &mut decoded, false)
                .map_err(codec_error)?;
            assert_eq!(length, samples);
            expected.extend_from_slice(&decoded[..length]);
            receiver.push(&RtpPacket::new(
                111,
                index as u16,
                (u32::MAX - 119).wrapping_add((index * samples) as u32),
                7,
                packet,
            )?)?;
        }
        assert!(
            expected
                .iter()
                .map(|sample| i64::from(*sample).pow(2))
                .sum::<i64>()
                > 1_000_000,
            "{samples} samples per packet"
        );
        for chunk in expected.as_chunks::<960>().0 {
            let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
            assert_eq!(frame.rate(), 48000);
            assert_eq!(frame.samples(), chunk);
        }
        assert!(receiver.pop()?.is_none());
    }
    Ok(())
}

#[test]
fn g711_packets_assemble_into_complete_twenty_millisecond_frames() -> Result<(), MediaError> {
    for (format, payload) in [(AudioFormat::Pcmu, 0), (AudioFormat::Pcma, 8)] {
        for sizes in [[80, 80, 80, 80], [240, 80, 160, 160], [480, 160, 320, 160]] {
            let mut receiver = RtpReceiver::new(format, payload)?;
            let mut timestamp = u32::MAX - 79;
            let mut expected = Vec::new();
            for (index, size) in sizes.into_iter().enumerate() {
                let sample = if index % 2 == 0 { 4000 } else { -7000 };
                let byte = match format {
                    AudioFormat::Pcmu => mulaw_encode(sample),
                    AudioFormat::Pcma => alaw_encode(sample),
                    AudioFormat::Opus => return Err(MediaError::Unsupported),
                };
                receiver.push(&RtpPacket::new(
                    payload,
                    index as u16,
                    timestamp,
                    7,
                    &vec![byte; size],
                )?)?;
                expected.extend(std::iter::repeat_n(sample, size));
                timestamp = timestamp.wrapping_add(size as u32);
            }
            for chunk in expected.as_chunks::<160>().0 {
                let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
                assert_eq!(frame.rate(), 8000);
                assert_eq!(frame.samples().len(), 160);
                assert!(frame
                    .samples()
                    .iter()
                    .zip(chunk)
                    .all(
                        |(actual, expected)| (i32::from(*actual) - i32::from(*expected)).abs()
                            < 300
                    ));
            }
            assert!(receiver.pop()?.is_none());
        }
    }
    Ok(())
}

#[test]
fn missing_samples_do_not_join_an_earlier_partial_frame() -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    for (sequence, timestamp, sample) in [(1, 0, 4000), (3, 160, -7000), (4, 240, -7000)] {
        receiver.push(&RtpPacket::new(
            0,
            sequence,
            timestamp,
            7,
            &[mulaw_encode(sample); 80],
        )?)?;
    }
    assert!(receiver.pop()?.is_none());
    let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
    assert_eq!(frame.samples().len(), 160);
    assert!(frame
        .samples()
        .iter()
        .all(|sample| (i32::from(*sample) + 7000).abs() < 300));
    assert!(receiver.pop()?.is_none());
    Ok(())
}

#[test]
fn two_hundred_millisecond_g711_packets_fit_the_bounded_transport() -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    receiver.push(&RtpPacket::new(0, 1, 0, 7, &[mulaw_encode(4000); 1600])?)?;
    for _ in 0..10 {
        let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
        assert_eq!(frame.samples().len(), 160);
        assert!(frame
            .samples()
            .iter()
            .all(|sample| (i32::from(*sample) - 4000).abs() < 150));
    }
    assert!(receiver.pop()?.is_none());
    Ok(())
}

#[test]
fn clearing_audio_discards_complete_and_partial_frames_without_changing_source(
) -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    receiver.push(&RtpPacket::new(0, 1, 0, 7, &[mulaw_encode(4000); 1600])?)?;
    assert!(receiver.pop()?.is_some());
    receiver.discard_pending();
    for _ in 0..12 {
        assert!(receiver.pop()?.is_none());
    }
    receiver.push(&RtpPacket::new(0, 2, 1600, 7, &[mulaw_encode(4000); 80])?)?;
    assert!(receiver.pop()?.is_none());
    receiver.discard_pending();
    assert!(receiver
        .push(&RtpPacket::new(0, 3, 1680, 8, &[mulaw_encode(-7000); 160])?)
        .is_err());
    receiver.push(&RtpPacket::new(0, 3, 1680, 7, &[mulaw_encode(-7000); 160])?)?;
    let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
    assert!(frame
        .samples()
        .iter()
        .all(|sample| (i32::from(*sample) + 7000).abs() < 300));
    Ok(())
}

#[test]
fn rejecting_a_long_packet_discards_partial_audio_and_keeps_the_stream_usable(
) -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    receiver.push(&RtpPacket::new(0, 1, 0, 7, &[mulaw_encode(4000); 80])?)?;
    receiver.push(&RtpPacket::new(0, 2, 80, 7, &[mulaw_encode(4000); 1601])?)?;
    for (sequence, timestamp) in [(3, 1681), (4, 1761)] {
        receiver.push(&RtpPacket::new(
            0,
            sequence,
            timestamp,
            7,
            &[mulaw_encode(-7000); 80],
        )?)?;
    }
    assert_eq!(receiver.pop(), Err(MediaError::Capacity));
    let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
    assert_eq!(frame.samples().len(), 160);
    assert!(frame
        .samples()
        .iter()
        .all(|sample| (i32::from(*sample) + 7000).abs() < 300));
    Ok(())
}
