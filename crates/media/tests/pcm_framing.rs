use rusterisk_media::{Frame, MediaError, PcmFramer};

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
