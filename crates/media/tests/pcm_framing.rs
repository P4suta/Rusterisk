use rusterisk_media::{AudioSocket, Frame, MediaError, PcmFramer, MAX_SAMPLES};

#[test]
fn each_audiosocket_rate_buffers_ten_frames_and_a_partial_frame_at_its_wire_rate(
) -> Result<(), MediaError> {
    for (tag, rate) in [
        (0x10, 8000),
        (0x11, 12000),
        (0x12, 16000),
        (0x13, 24000),
        (0x14, 32000),
        (0x15, 44100),
        (0x16, 48000),
    ] {
        let samples = rate as usize / 50;
        let mut buffer = None;
        let allocation = allocation_counter::measure(|| {
            buffer = Some(PcmFramer::new(rate));
        });
        assert!(
            allocation.bytes_total <= (11 * samples * size_of::<i16>()) as u64,
            "{rate} Hz reserved {} bytes",
            allocation.bytes_total
        );
        let mut buffer = buffer.ok_or(MediaError::Capacity)??;
        let audio = Frame::new(&[157; MAX_SAMPLES][..samples], rate)?;
        let bytes = AudioSocket::Audio(audio).encode()?;
        assert_eq!(bytes[0], tag);
        assert_eq!(
            u16::from_be_bytes([bytes[1], bytes[2]]) as usize,
            samples * 2
        );
        let AudioSocket::Audio(decoded) = AudioSocket::parse(&bytes)? else {
            return Err(MediaError::InvalidPacket);
        };
        for _ in 0..10 {
            buffer.push(decoded)?;
        }
        buffer.push(Frame::new(&[94; MAX_SAMPLES][..samples - 1], rate)?)?;
        assert_eq!(
            buffer.push(Frame::new(&[22], rate)?),
            Err(MediaError::Capacity)
        );
        for _ in 0..10 {
            assert_eq!(buffer.pop(), Some(audio));
        }
        buffer.push(Frame::new(&[22], rate)?)?;
        let last = buffer.pop().ok_or(MediaError::InvalidFrame)?;
        assert_eq!(last.samples()[samples - 1], 22);
        assert!(last.samples()[..samples - 1]
            .iter()
            .all(|value| *value == 94));
    }
    Ok(())
}

#[test]
fn continuous_pcm_chunks_form_exact_twenty_millisecond_frames() -> Result<(), MediaError> {
    let mut framer = PcmFramer::new(8000)?;
    framer.push(Frame::new(&[100; 79], 8000)?)?;
    assert!(framer.pop().is_none());
    framer.push(Frame::new(&[200; 401], 8000)?)?;
    let first = framer.pop().ok_or(MediaError::Capacity)?;
    assert_eq!(&first.samples()[..79], &[100; 79]);
    assert_eq!(&first.samples()[79..], &[200; 81]);
    for _ in 0..2 {
        assert_eq!(
            framer.pop().ok_or(MediaError::Capacity)?.samples(),
            &[200; 160]
        );
    }
    assert!(framer.pop().is_none());
    framer.push(Frame::new(&[300; 80], 8000)?)?;
    framer.clear();
    framer.push(Frame::new(&[400; 160], 8000)?)?;
    assert_eq!(
        framer.pop().ok_or(MediaError::Capacity)?.samples(),
        &[400; 160]
    );
    Ok(())
}

#[test]
fn pcm_capacity_and_rate_failures_preserve_queued_audio() -> Result<(), MediaError> {
    let mut framer = PcmFramer::new(8000)?;
    for _ in 0..10 {
        framer.push(Frame::new(&[1234; 160], 8000)?)?;
    }
    assert!(matches!(
        framer.push(Frame::new(&[999; 160], 8000)?),
        Err(MediaError::Capacity)
    ));
    assert!(matches!(
        framer.push(Frame::new(&[999; 160], 16000)?),
        Err(MediaError::InvalidFrame)
    ));
    for _ in 0..10 {
        assert_eq!(
            framer.pop().ok_or(MediaError::Capacity)?.samples(),
            &[1234; 160]
        );
    }
    assert!(framer.pop().is_none());
    assert!(matches!(PcmFramer::new(0), Err(MediaError::InvalidFrame)));
    Ok(())
}
