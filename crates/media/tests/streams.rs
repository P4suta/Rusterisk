use rusterisk_media::{AudioFormat, Frame, MediaError, RtpPacket, RtpReceiver, RtpSender};

#[test]
fn packet_clocks_advance_by_audio_samples_and_receivers_restore_wrapping_packet_order(
) -> Result<(), MediaError> {
    let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [0, 0, 0, 7, 255, 254, 0, 0, 0, 9])?;
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    let mut packets = Vec::new();
    for value in [1000, 2000, 3000, 4000] {
        let packet = sender.packet(Frame::new(&[value; 160], 8000)?)?;
        packets.push(RtpPacket::parse(&packet.encode()?)?);
    }
    assert_eq!(packets[0].sequence(), 65534);
    assert_eq!(packets[2].sequence(), 0);
    assert_eq!(packets[3].timestamp(), 9 + 3 * 160);
    for index in [0, 2, 1, 3, 1] {
        receiver.push(&packets[index])?;
    }
    for expected in [1000i32, 2000, 3000, 4000] {
        let frame = receiver.pop()?.ok_or(MediaError::InvalidFrame)?;
        assert!((i32::from(frame.samples()[0]) - expected).abs() < 150);
    }
    assert!(receiver.pop()?.is_none());
    receiver.push(&packets[0])?;
    assert!(receiver.pop()?.is_none());
    Ok(())
}

#[test]
fn unrelated_sources_and_payloads_cannot_replace_a_negotiated_audio_stream(
) -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcma, 8)?;
    receiver.push(&RtpPacket::new(8, 1, 0, 7, &[0xd5; 160])?)?;
    assert!(receiver
        .push(&RtpPacket::new(0, 2, 160, 7, &[0xff; 160])?)
        .is_err());
    assert!(receiver
        .push(&RtpPacket::new(8, 2, 160, 8, &[0xd5; 160])?)
        .is_err());
    assert!(receiver
        .push(&RtpPacket::new(8, 40, 6240, 7, &[0xd5; 160])?)
        .is_err());
    assert!(receiver.pop()?.is_some());
    Ok(())
}

#[test]
fn silence_suppression_does_not_advance_packet_numbers_without_received_packets(
) -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    receiver.push(&RtpPacket::new(0, 1, 0, 7, &[0xff; 160])?)?;
    assert!(receiver.pop()?.is_some());
    for _ in 0..100 {
        assert!(receiver.pop()?.is_none());
    }
    receiver.push(&RtpPacket::new(0, 2, 16000, 7, &[0xff; 160])?)?;
    assert!(receiver.pop()?.is_some());
    receiver.push(&RtpPacket::new(0, 200, 32000, 7, &[0xff; 160])?)?;
    assert!(receiver.pop()?.is_some());
    Ok(())
}

#[test]
fn telephone_packets_share_sequence_numbers_without_delaying_audio() -> Result<(), MediaError> {
    let mut receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    let event = RtpPacket::new(101, 65535, 0, 7, &[1, 0, 0, 160])?;
    assert!(!receiver.skip(&event)?);
    receiver.push(&RtpPacket::new(0, 65534, 0, 7, &[0xff; 160])?)?;
    receiver.push(&RtpPacket::new(0, 1, 160, 7, &[0x80; 160])?)?;
    assert!(!receiver.skip(&RtpPacket::new(101, 65535, 0, 8, event.payload())?)?);
    assert!(receiver.skip(&event)?);
    assert!(receiver.skip(&RtpPacket::new(101, 0, 0, 7, event.payload())?)?);
    assert!(!receiver.skip(&event)?);
    assert_eq!(
        receiver.pop()?.ok_or(MediaError::InvalidFrame)?.samples()[0],
        0
    );
    assert!(receiver.pop()?.ok_or(MediaError::InvalidFrame)?.samples()[0] > 30000);
    assert!(receiver.pop()?.is_none());
    assert!(!receiver.skip(&event)?);
    Ok(())
}
