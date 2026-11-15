use rusterisk_api::MediaFormat;
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::media_websocket::{BufferItem, BufferNotice, FlowNotice, MediaBuffer};

#[test]
fn supported_wire_formats_decode_exact_sample_rates_and_values() -> Result<(), MediaError> {
    for format in [
        MediaFormat::Pcmu,
        MediaFormat::Pcma,
        MediaFormat::Pcm8,
        MediaFormat::Pcm12,
        MediaFormat::Pcm16,
        MediaFormat::Pcm24,
        MediaFormat::Pcm32,
        MediaFormat::Pcm44,
        MediaFormat::Pcm48,
    ] {
        let mut buffer = MediaBuffer::new(format)?;
        let count = usize::from(format.optimal_frame_size());
        let (bytes, sample) = match format {
            MediaFormat::Pcmu => (vec![0xff; count], 0),
            MediaFormat::Pcma => (vec![0xd5; count], 8),
            MediaFormat::Pcm8
            | MediaFormat::Pcm12
            | MediaFormat::Pcm16
            | MediaFormat::Pcm24
            | MediaFormat::Pcm32
            | MediaFormat::Pcm44
            | MediaFormat::Pcm48 => ([0x34, 0x12].repeat(count / 2), 0x1234),
            MediaFormat::Opus => return Err(MediaError::Unsupported),
        };
        buffer.push(&bytes)?;
        let Some(BufferItem::Frame(frame)) = buffer.pop().item else {
            return Err(MediaError::InvalidFrame);
        };
        assert_eq!(frame.rate(), format.rate());
        assert_eq!(frame.samples().len(), format.rate() as usize / 50);
        assert!(frame.samples().iter().all(|value| *value == sample));
    }
    Ok(())
}

#[test]
fn failed_bulk_completion_keeps_partial_audio_and_transfer_state() -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Pcm8)?;
    buffer.start_buffering();
    buffer.push(&[0x34, 0x12].repeat(40))?;
    for _ in 0..128 {
        buffer.mark("pending".into())?;
    }
    assert!(matches!(
        buffer.finish_buffering("transfer".into()),
        Err(MediaError::Capacity)
    ));
    assert!(buffer.is_buffering());
    assert_eq!(buffer.queued_frames(), 0);
    assert!(matches!(
        buffer.pop().item,
        Some(BufferItem::Notice(BufferNotice::Mark(_)))
    ));
    let _flow = buffer.finish_buffering("transfer".into())?;
    for _ in 0..127 {
        assert!(matches!(
            buffer.pop().item,
            Some(BufferItem::Notice(BufferNotice::Mark(_)))
        ));
    }
    let Some(BufferItem::Frame(frame)) = buffer.pop().item else {
        return Err(MediaError::InvalidFrame);
    };
    assert_eq!(&frame.samples()[..40], &[0x1234; 40]);
    assert_eq!(&frame.samples()[40..], &[0; 120]);
    assert!(
        matches!(buffer.pop().item, Some(BufferItem::Notice(BufferNotice::BufferingCompleted(id))) if id == "transfer")
    );
    assert!(buffer.is_empty());
    Ok(())
}

#[test]
fn bulk_messages_preserve_partial_samples_and_complete_in_queue_order() -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Pcm8)?;
    buffer.start_buffering();
    assert_eq!(buffer.push(&[0xa0; 79])?, None);
    assert_eq!(buffer.push(&[0x0f; 241])?, None);
    buffer.mark("position".into())?;
    buffer.push(&[0x10; 2])?;
    let _flow = buffer.finish_buffering("transfer".into())?;
    buffer.report_drained();
    let first = buffer.pop();
    let Some(BufferItem::Frame(frame)) = first.item else {
        return Err(MediaError::InvalidFrame);
    };
    assert_eq!(frame.samples()[0], i16::from_le_bytes([0xa0, 0xa0]));
    assert!(
        matches!(buffer.pop().item, Some(BufferItem::Notice(BufferNotice::Mark(id))) if id == "position")
    );
    let final_audio = buffer.pop();
    let Some(BufferItem::Frame(frame)) = final_audio.item else {
        return Err(MediaError::InvalidFrame);
    };
    assert_eq!(frame.samples()[0], 0x1010);
    assert_eq!(&frame.samples()[1..], &[0; 159]);
    let completion = buffer.pop();
    assert!(
        matches!(completion.item, Some(BufferItem::Notice(BufferNotice::BufferingCompleted(id))) if id == "transfer")
    );
    assert!(!completion.drained);
    assert!(buffer.pop().drained);
    assert!(!buffer.pop().drained);
    Ok(())
}

#[test]
fn flush_preserves_playout_and_discards_audio_and_marks() -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Pcmu)?;
    buffer.push(&[0xff; 159])?;
    assert!(buffer.pop().item.is_none());
    buffer.push(&[0xff; 160])?;
    buffer.pause();
    assert!(buffer.pop().item.is_none());
    buffer.resume();
    assert!(
        matches!(buffer.pop().item, Some(BufferItem::Frame(frame)) if frame == Frame::new(&[0; 160], 8000)?)
    );
    buffer.start_buffering();
    buffer.push(&[0xff; 80])?;
    buffer.mark("discarded".into())?;
    buffer.pause();
    buffer.report_drained();
    let _flow = buffer.flush();
    assert!(!buffer.is_buffering());
    assert!(buffer.is_paused());
    let empty = buffer.pop();
    assert!(empty.item.is_none());
    assert!(!empty.drained);
    buffer.resume();
    let empty = buffer.pop();
    assert!(empty.item.is_none());
    assert!(empty.drained);
    Ok(())
}

#[test]
fn backpressure_and_capacity_failures_preserve_queued_samples() -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Pcmu)?;
    for _ in 0..899 {
        assert_eq!(buffer.push(&[0xff; 160])?, None);
    }
    assert!(matches!(
        buffer.push(&[0xff; 65500]),
        Err(MediaError::Capacity)
    ));
    assert_eq!(buffer.queued_frames(), 899);
    assert!(!buffer.is_full());
    assert_eq!(buffer.push(&[0xff; 160])?, Some(FlowNotice::Xoff));
    assert!(matches!(buffer.push(&[0; 160]), Err(MediaError::Capacity)));
    assert_eq!(buffer.queued_frames(), 900);
    for _ in 0..101 {
        assert_eq!(buffer.pop().flow, None);
    }
    assert_eq!(buffer.pop().flow, Some(FlowNotice::Xon));
    assert_eq!(buffer.queued_frames(), 798);
    assert!(matches!(
        buffer.push(&[0; 65501]),
        Err(MediaError::Capacity)
    ));
    assert_eq!(buffer.queued_frames(), 798);
    for _ in 0..128 {
        buffer.mark("marker".into())?;
    }
    assert!(matches!(
        buffer.mark("excess".into()),
        Err(MediaError::Capacity)
    ));
    assert_eq!(buffer.queued_frames(), 798);
    Ok(())
}

#[test]
fn paused_flush_defers_xon_and_drained_notifications_until_playout_resumes(
) -> Result<(), MediaError> {
    let mut buffer = MediaBuffer::new(MediaFormat::Pcmu)?;
    for _ in 0..899 {
        assert_eq!(buffer.push(&[0xff; 160])?, None);
    }
    assert_eq!(buffer.push(&[0xff; 160])?, Some(FlowNotice::Xoff));
    buffer.pause();
    buffer.report_drained();
    assert_eq!(buffer.flush(), None);
    assert!(buffer.is_paused());
    assert!(buffer.is_full());
    assert_eq!(buffer.queued_frames(), 0);
    let paused = buffer.pop();
    assert_eq!(paused.flow, None);
    assert!(!paused.drained);
    buffer.resume();
    let resumed = buffer.pop();
    assert_eq!(resumed.flow, Some(FlowNotice::Xon));
    assert!(resumed.drained);
    assert!(!buffer.is_full());
    assert_eq!(buffer.pop().flow, None);
    Ok(())
}
