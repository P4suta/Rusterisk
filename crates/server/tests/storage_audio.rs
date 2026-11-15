use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{StorageWorker, WorkerError};

fn id(value: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(value.to_owned()).map_err(|_| WorkerError::Invalid)
}

fn frame(sample: i16) -> Result<Frame, WorkerError> {
    Frame::new(&[sample; 160], 8000).map_err(|error| WorkerError::Storage(error.into()))
}

#[tokio::test]
async fn finishing_a_recording_drains_accepted_audio_and_publishes_its_wave_file(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let mut recording = port.record(id("received")?, 8000).await?;
    recording.try_write(frame(1234)?)?;
    recording.try_write(frame(-4321)?)?;
    recording.finish().await?;
    let bytes = std::fs::read(directory.path().join("recordings/received.wav"))?;
    assert_eq!(bytes.len(), 44 + 640);
    for (samples, expected) in [(&bytes[44..364], 1234i16), (&bytes[364..], -4321i16)] {
        assert!(samples
            .as_chunks::<2>()
            .0
            .iter()
            .all(|sample| i16::from_le_bytes(*sample) == expected));
    }
    let recordings = port.recordings().await?;
    assert_eq!(recordings.len(), 1);
    assert_eq!(recordings[0].name, "received");
    assert!(!directory
        .path()
        .join("recordings/received.partial")
        .exists());
    worker.close().await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_or_dropped_recordings_remove_their_files_and_release_their_names(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let mut cancelled = port.record(id("discarded")?, 8000).await?;
    cancelled.try_write(frame(2000)?)?;
    cancelled.cancel().await?;
    let mut dropped = port.record(id("discarded")?, 8000).await?;
    dropped.try_write(frame(3000)?)?;
    drop(dropped);
    assert!(port.recordings().await?.is_empty());
    assert!(!directory
        .path()
        .join("recordings/discarded.partial")
        .exists());
    let fresh = port.record(id("discarded")?, 8000).await?;
    fresh.cancel().await?;
    worker.close().await?;
    Ok(())
}
