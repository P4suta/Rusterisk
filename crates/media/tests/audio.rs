use rusterisk_media::{
    alaw_decode, alaw_encode, mulaw_decode, mulaw_encode, AudioSocket, Frame, MediaError,
    MediaQueue, OpusCodec, RtpPacket,
};

#[test]
fn g711_matches_silence_and_quantized_roundtrips() {
    assert_eq!(mulaw_decode(0xff), 0);
    assert_eq!(alaw_decode(0xd5), 8);
    assert_eq!(mulaw_encode(0), 0xff);
    assert_eq!(alaw_encode(0), 0xd5);
    for byte in 0u8..=255 {
        assert!(
            (i32::from(mulaw_decode(mulaw_encode(mulaw_decode(byte))))
                - i32::from(mulaw_decode(byte)))
            .abs()
                <= 8
        );
        assert_eq!(
            alaw_decode(alaw_encode(alaw_decode(byte))),
            alaw_decode(byte)
        );
    }
}

#[test]
fn bounded_media_queue_preserves_order_and_flushes_interruptions() -> Result<(), MediaError> {
    let frame = Frame::new(&[42; 160], 8000)?;
    let mut queue = MediaQueue::new();
    for _ in 0..50 {
        queue.push(frame)?;
    }
    assert_eq!(queue.push(frame), Err(MediaError::Capacity));
    queue.pause();
    assert_eq!(queue.pop(), None);
    queue.resume();
    assert_eq!(queue.pop(), Some(frame));
    queue.flush();
    assert_eq!(queue.pop(), None);
    Ok(())
}

#[test]
fn rtp_and_audiosocket_preserve_real_audio_samples() -> Result<(), MediaError> {
    let payload: Vec<u8> = (0..160).map(|index| mulaw_encode(index * 100)).collect();
    let packet = RtpPacket::new(0, 65535, u32::MAX - 160, 17, &payload)?;
    let wire = packet.encode()?;
    let decoded = RtpPacket::parse(&wire)?;
    assert_eq!(decoded.payload(), payload);
    assert_eq!(decoded.sequence(), 65535);
    let frame = Frame::new(&[1234; 160], 8000)?;
    let message = AudioSocket::Audio(frame);
    assert_eq!(AudioSocket::parse(&message.encode()?)?, message);
    assert!(RtpPacket::parse(&wire[..10]).is_err());
    assert!(AudioSocket::parse(&[0x10, 0, 4, 0]).is_err());
    Ok(())
}

#[test]
fn opus_transmits_a_non_silent_known_signal() -> Result<(), MediaError> {
    let mut codec = OpusCodec::new()?;
    let mut samples = [0i16; 960];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = if index % 96 < 48 { 5000 } else { -5000 };
    }
    let input = Frame::new(&samples, 48000)?;
    let wire = codec.encode(input)?;
    let output = codec.decode(&wire)?;
    assert_eq!(output.samples().len(), 960);
    assert!(
        output
            .samples()
            .iter()
            .map(|value| i64::from(*value).pow(2))
            .sum::<i64>()
            > 1_000_000
    );
    Ok(())
}

#[test]
fn rtp_padding_must_have_a_positive_length() {
    let invalid = [0xa0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 17, 0xff, 0];
    assert!(RtpPacket::parse(&invalid).is_err());
}

#[test]
fn secure_rtp_matches_libsrtp_wire_bytes() -> Result<(), MediaError> {
    use rusterisk_media::Srtp;

    let key = [
        0xe1, 0xf9, 0x7a, 0x0d, 0x3e, 0x01, 0x8b, 0xe0, 0xd6, 0x4f, 0xa3, 0x2c, 0x06, 0xde, 0x41,
        0x39,
    ];
    let salt = [
        0x0e, 0xc6, 0x75, 0xad, 0x49, 0x8a, 0xfe, 0xeb, 0xb6, 0x96, 0x0b, 0x3a, 0xab, 0xe6,
    ];
    let expected = [
        0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe, 0x4e, 0x55, 0xdc,
        0x4c, 0xe7, 0x99, 0x78, 0xd8, 0x8c, 0xa4, 0xd2, 0x15, 0x94, 0x9d, 0x24, 0x02, 0xb7, 0x8d,
        0x6a, 0xcc, 0x99, 0xea, 0x17, 0x9b, 0x8d, 0xbb,
    ];
    let plaintext = RtpPacket::new(15, 0x1234, 0xdeca_fbad, 0xcafe_babe, &[0xab; 16])?.encode()?;
    let ciphertext = Srtp::new(&key, &salt)?.encrypt(&plaintext)?;
    assert_eq!(ciphertext.as_ref(), expected);
    assert_eq!(Srtp::new(&key, &salt)?.decrypt(&expected)?, plaintext);
    Ok(())
}

#[test]
fn secure_rtp_authenticates_audio_and_rejects_replay() -> Result<(), MediaError> {
    let mut sender = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    let mut receiver = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    let packet = RtpPacket::new(0, 42, 160, 99, &[0xff; 160])?.encode()?;
    let encrypted = sender.encrypt(&packet)?;
    assert_ne!(encrypted, packet);
    assert_eq!(receiver.decrypt(&encrypted)?, packet);
    assert!(receiver.decrypt(&encrypted).is_err());
    let mut tampered = encrypted.to_vec();
    tampered[12] ^= 1;
    assert!(receiver.decrypt(&tampered).is_err());
    Ok(())
}

#[test]
fn concrete_rtcp_writers_form_a_borrowed_compound_packet() -> Result<(), MediaError> {
    let bytes = rusterisk_media::sender_report(99, 12345, 160, 1, 160, "test")?;
    assert_eq!(
        rusterisk_media::inspect_rtcp(&bytes)?,
        [
            rusterisk_media::ControlKind::Sender,
            rusterisk_media::ControlKind::Description
        ]
    );
    assert!(rusterisk_media::inspect_rtcp(&bytes[..bytes.len() - 1]).is_err());
    Ok(())
}

#[test]
fn secure_rtp_accepts_sequence_wraparound_and_preserves_replay_protection() -> Result<(), MediaError>
{
    let mut sender = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    let mut receiver = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    for sequence in [65534, 65535, 0, 1] {
        let packet = RtpPacket::new(0, sequence, 160, 99, &[0xff; 160])?.encode()?;
        let encrypted = sender.encrypt(&packet)?;
        assert_eq!(receiver.decrypt(&encrypted)?, packet);
        assert!(receiver.decrypt(&encrypted).is_err());
    }
    Ok(())
}

#[test]
fn unauthenticated_streams_consume_no_slots_and_authenticated_streams_are_bounded(
) -> Result<(), MediaError> {
    let mut receiver = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    for ssrc in 0..16 {
        let mut sender = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
        let packet = RtpPacket::new(0, 42, 160, ssrc, &[0xff; 160])?.encode()?;
        let mut encrypted = sender.encrypt(&packet)?.to_vec();
        encrypted[12] ^= 1;
        assert!(receiver.decrypt(&encrypted).is_err());
    }
    for ssrc in 16..20 {
        let mut sender = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
        let packet = RtpPacket::new(0, 42, 160, ssrc, &[0xff; 160])?.encode()?;
        assert_eq!(receiver.decrypt(&sender.encrypt(&packet)?)?, packet);
    }
    let mut sender = rusterisk_media::Srtp::new(&[7; 16], &[9; 14])?;
    let packet = RtpPacket::new(0, 42, 160, 20, &[0xff; 160])?.encode()?;
    assert_eq!(
        receiver.decrypt(&sender.encrypt(&packet)?),
        Err(MediaError::Capacity)
    );
    Ok(())
}
