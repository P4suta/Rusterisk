use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{PlaybackMedia, StorageWorker, WorkerError};
use rusterisk_storage::StorageError;

fn id(value: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(value.to_owned()).map_err(|_| WorkerError::Invalid)
}

#[tokio::test]
async fn recorded_audio_is_read_in_order_through_the_bounded_worker() -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let mut recording = port.record(id("message")?, 8000).await?;
    for sample in [1200, -3400] {
        recording.try_write(Frame::new(&[sample; 160], 8000).map_err(StorageError::from)?)?;
    }
    recording.finish().await?;
    let mut source = port.play(PlaybackMedia::Recording(id("message")?)).await?;
    for expected in [1200, -3400] {
        let frame = source.read().await?.ok_or(WorkerError::Invalid)?;
        assert_eq!(frame.rate(), 8000);
        assert_eq!(frame.samples(), &[expected; 160]);
    }
    assert!(source.read().await?.is_none());
    assert!(source.read().await?.is_none());
    drop(source);
    worker.close().await?;
    Ok(())
}

#[tokio::test]
async fn missing_and_invalid_sounds_return_errors_without_stopping_storage(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    assert!(matches!(
        port.play(PlaybackMedia::Recording(id("missing")?)).await,
        Err(WorkerError::Storage(StorageError::Io(_)))
    ));
    std::fs::create_dir(directory.path().join("sounds"))?;
    std::fs::write(directory.path().join("sounds/broken.wav"), b"invalid")?;
    assert!(matches!(
        port.play(PlaybackMedia::Sound(id("broken")?)).await,
        Err(WorkerError::Storage(StorageError::Io(_)))
    ));
    assert!(port.recordings().await?.is_empty());
    worker.close().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn playback_rejects_a_sound_directory_that_is_a_symbolic_link() -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let mut recording = rusterisk_storage::Recorder::create(outside.path(), "outside", 8000)?;
    recording.write(Frame::new(&[10; 160], 8000).map_err(StorageError::from)?)?;
    recording.finish()?;
    std::os::unix::fs::symlink(
        outside.path().join("recordings"),
        directory.path().join("sounds"),
    )?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = port.play(PlaybackMedia::Sound(id("outside")?)).await;
    worker.close().await?;
    assert!(matches!(
        result,
        Err(WorkerError::Storage(StorageError::InvalidName))
    ));
    Ok(())
}
