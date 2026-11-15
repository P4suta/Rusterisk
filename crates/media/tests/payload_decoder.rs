use rusterisk_media::{alaw_encode, mulaw_encode, AudioDecoder, AudioFormat, MediaError};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Codec(error.to_string())
}

fn opus_packet(samples: usize, frames: usize) -> Result<Vec<u8>, MediaError> {
    let mut encoder = opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Audio)
        .map_err(failure)?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(16000))
        .map_err(failure)?;
    let mut output = [0; 1275];
    let input = vec![0; samples];
    let size = encoder.encode(&input, &mut output).map_err(failure)?;
    let packet = &output[..size];
    let mut parts = Vec::with_capacity(frames);
    for _ in 0..frames {
        parts.push(packet);
    }
    let mut combined = [0; 2036];
    let size = opus::Repacketizer::new()
        .map_err(failure)?
        .combine(&parts, &mut combined)
        .map_err(failure)?;
    Ok(combined[..size].to_vec())
}

#[test]
fn g711_payloads_keep_partial_samples_and_emit_fixed_twenty_millisecond_frames(
) -> Result<(), MediaError> {
    let mut decoder = AudioDecoder::new(AudioFormat::Pcmu)?;
    decoder.push(&[mulaw_encode(4000); 80])?;
    assert!(decoder.pop().is_none());
    decoder.push(&[mulaw_encode(4000); 400])?;
    for _ in 0..3 {
        let frame = decoder.pop().ok_or(MediaError::InvalidFrame)?;
        assert_eq!(frame.rate(), 8000);
        assert_eq!(frame.samples().len(), 160);
        assert!(frame
            .samples()
            .iter()
            .all(|sample| (i32::from(*sample) - 4000).abs() < 300));
    }
    assert!(decoder.pop().is_none());
    Ok(())
}

#[test]
fn opus_packets_cover_each_frame_duration_and_the_full_120_millisecond_packet(
) -> Result<(), MediaError> {
    for (samples, count) in [(120, 8), (240, 4), (480, 2), (960, 1), (1920, 1), (2880, 1)] {
        let packet = opus_packet(samples, 1)?;
        let mut decoder = AudioDecoder::new(AudioFormat::Opus)?;
        for _ in 0..count {
            decoder.push(&packet)?;
        }
        for _ in 0..(samples * count / 960) {
            let frame = decoder.pop().ok_or(MediaError::InvalidFrame)?;
            assert_eq!(frame.rate(), 48000);
            assert_eq!(frame.samples().len(), 960);
        }
        assert!(decoder.pop().is_none());
    }
    for (samples, frames) in [
        (120, 48),
        (240, 24),
        (480, 12),
        (960, 6),
        (1920, 3),
        (2880, 2),
    ] {
        let mut decoder = AudioDecoder::new(AudioFormat::Opus)?;
        decoder.push(&opus_packet(samples, frames)?)?;
        for _ in 0..6 {
            assert_eq!(
                decoder
                    .pop()
                    .ok_or(MediaError::InvalidFrame)?
                    .samples()
                    .len(),
                960
            );
        }
        assert!(decoder.pop().is_none());
    }
    Ok(())
}

#[test]
fn cancellation_and_format_changes_discard_ready_frames_and_partial_samples(
) -> Result<(), MediaError> {
    let mut decoder = AudioDecoder::new(AudioFormat::Pcmu)?;
    decoder.push(&[mulaw_encode(4000); 240])?;
    decoder.clear();
    assert!(decoder.pop().is_none());
    decoder.push(&[mulaw_encode(4000); 80])?;
    assert!(decoder.pop().is_none());
    decoder.reset(AudioFormat::Pcma)?;
    decoder.push(&[alaw_encode(-7000); 80])?;
    assert!(decoder.pop().is_none());
    decoder.push(&[alaw_encode(-7000); 80])?;
    let frame = decoder.pop().ok_or(MediaError::InvalidFrame)?;
    assert!(frame
        .samples()
        .iter()
        .all(|sample| (i32::from(*sample) + 7000).abs() < 300));
    decoder.push(&[alaw_encode(-7000); 240])?;
    decoder.reset(AudioFormat::Opus)?;
    assert!(decoder.pop().is_none());
    decoder.push(&opus_packet(480, 1)?)?;
    assert!(decoder.pop().is_none());
    decoder.clear();
    let packet = opus_packet(480, 1)?;
    decoder.push(&packet)?;
    assert!(decoder.pop().is_none());
    decoder.push(&packet)?;
    assert_eq!(
        decoder
            .pop()
            .ok_or(MediaError::InvalidFrame)?
            .samples()
            .len(),
        960
    );
    Ok(())
}

#[test]
fn queue_admission_rejects_a_whole_payload_without_leaving_partial_output() -> Result<(), MediaError>
{
    let mut decoder = AudioDecoder::new(AudioFormat::Opus)?;
    let packet = opus_packet(960, 6)?;
    decoder.push(&packet)?;
    assert!(matches!(decoder.push(&packet), Err(MediaError::Capacity)));
    for _ in 0..6 {
        assert!(decoder.pop().is_some());
    }
    assert!(decoder.pop().is_none());
    decoder.reset(AudioFormat::Pcmu)?;
    decoder.push(&[0xff; 1600])?;
    assert!(matches!(
        decoder.push(&[0xff; 160]),
        Err(MediaError::Capacity)
    ));
    for _ in 0..10 {
        assert!(decoder.pop().is_some());
    }
    assert!(decoder.pop().is_none());
    decoder.push(&[0xff; 80])?;
    assert!(decoder.pop().is_none());
    assert!(matches!(decoder.push(&[]), Err(MediaError::InvalidPacket)));
    assert!(matches!(
        decoder.push(&[0xff; 1601]),
        Err(MediaError::Capacity)
    ));
    Ok(())
}
