use rusterisk_api::MediaFormat;
use rusterisk_media::MediaError;
use rusterisk_server::media_websocket::{BufferItem, MediaBuffer};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Codec(error.to_string())
}

fn packet(samples: usize, parts: usize) -> Result<Vec<u8>, MediaError> {
    let mut encoder = opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Audio)
        .map_err(failure)?;
    let mut encoded = [0; 1275];
    let size = encoder
        .encode(&vec![0; samples], &mut encoded)
        .map_err(failure)?;
    let packets: Vec<&[u8]> = (0..parts).map(|_| &encoded[..size]).collect();
    let mut output = [0; 2036];
    let size = opus::Repacketizer::new()
        .map_err(failure)?
        .combine(&packets, &mut output)
        .map_err(failure)?;
    Ok(output[..size].to_vec())
}

#[test]
fn complete_opus_messages_decode_short_and_long_packet_durations_without_byte_reframing(
) -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Opus)?;
    buffer.push(&packet(480, 1)?)?;
    assert_eq!(buffer.queued_frames(), 0);
    assert!(buffer.pop().item.is_none());
    buffer.push(&packet(480, 1)?)?;
    assert!(
        matches!(buffer.pop().item, Some(BufferItem::Frame(frame)) if frame.rate() == 48000 && frame.samples().len() == 960)
    );
    buffer.push(&packet(2880, 2)?)?;
    assert_eq!(buffer.queued_frames(), 6);
    for _ in 0..6 {
        assert!(
            matches!(buffer.pop().item, Some(BufferItem::Frame(frame)) if frame.rate() == 48000 && frame.samples().len() == 960)
        );
    }
    assert!(buffer.pop().item.is_none());
    assert!(buffer.push(&[]).is_err());
    assert!(buffer.push(&[0; 2037]).is_err());
    assert_eq!(buffer.queued_frames(), 0);
    Ok(())
}

#[test]
fn flushing_packet_audio_discards_partial_decoded_frames() -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Opus)?;
    let short = packet(480, 1)?;
    buffer.push(&short)?;
    assert_eq!(buffer.flush(), None);
    buffer.push(&short)?;
    assert_eq!(buffer.queued_frames(), 0);
    buffer.push(&short)?;
    assert_eq!(buffer.queued_frames(), 1);
    Ok(())
}
