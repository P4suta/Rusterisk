use rusterisk_media::{AudioSocket, AudioSocketDecoder, Frame, MediaError};

#[test]
fn fragmented_stream_messages_preserve_headers_payloads_and_message_boundaries(
) -> Result<(), MediaError> {
    let messages = [
        AudioSocket::Uuid([7; 16]),
        AudioSocket::Audio(Frame::new(&[1234; 160], 8000)?),
        AudioSocket::Dtmf(b'#'),
        AudioSocket::Terminate,
    ];
    let mut bytes = Vec::new();
    for message in &messages {
        bytes.extend(message.encode()?);
    }
    for chunk_size in [1, 2, 3, 7, 19, bytes.len()] {
        let mut decoder = AudioSocketDecoder::new();
        let mut decoded = Vec::new();
        for chunk in bytes.chunks(chunk_size) {
            let mut offset = 0;
            while offset < chunk.len() {
                let (consumed, message) = decoder.push(&chunk[offset..])?;
                assert!(consumed > 0);
                offset += consumed;
                if let Some(message) = message {
                    decoded.push(message);
                }
            }
        }
        assert_eq!(decoded, messages);
        assert!(decoder.is_empty());
    }
    Ok(())
}

#[test]
fn stream_lengths_and_incomplete_messages_stay_bounded() -> Result<(), MediaError> {
    let mut decoder = AudioSocketDecoder::new();
    assert_eq!(decoder.push(&[])?, (0, None));
    assert_eq!(decoder.push(&[0x10, 0])?, (2, None));
    assert!(!decoder.is_empty());
    assert_eq!(decoder.push(&[2, 1])?, (2, None));
    let (consumed, message) = decoder.push(&[2, 0, 0, 0])?;
    assert_eq!(consumed, 1);
    assert_eq!(message, Some(AudioSocket::Audio(Frame::new(&[513], 8000)?)));
    assert!(decoder.is_empty());
    assert!(matches!(
        decoder.push(&[0x10, 0xff, 0xff]),
        Err(MediaError::Capacity)
    ));
    Ok(())
}
