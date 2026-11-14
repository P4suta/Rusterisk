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
