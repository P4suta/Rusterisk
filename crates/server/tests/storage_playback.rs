use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{PlaybackMedia, StorageWorker, WorkerError};
use rusterisk_storage::StorageError;

fn id(value: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(value.to_owned()).map_err(|_| WorkerError::Invalid)
}

#[tokio::test]
async fn nested_sounds_keep_literal_dots_and_return_the_stored_audio() -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let staging = tempfile::tempdir()?;
    let mut recording = rusterisk_storage::Recorder::create(staging.path(), "one.two", 8000)?;
    recording.write(Frame::new(&[2345; 160], 8000).map_err(StorageError::from)?)?;
    let path = recording.finish()?;
    std::fs::create_dir_all(directory.path().join("sounds/digits"))?;
    std::fs::rename(path, directory.path().join("sounds/digits/one.two.wav"))?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = async {
        let sound = rusterisk_api::SoundId::try_from("digits/one.two".to_owned())
            .map_err(|_| WorkerError::Invalid)?;
        let mut source = port.play(PlaybackMedia::Sound(sound)).await?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[2345; 160]
        );
        assert!(source.read().await?.is_none());
        Ok::<(), WorkerError>(())
    }
    .await;
    let cleanup = worker.close().await;
    result.and(cleanup)
}

#[cfg(unix)]
#[tokio::test]
async fn nested_sound_ancestors_cannot_redirect_playback_outside_the_library(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let mut recording = rusterisk_storage::Recorder::create(outside.path(), "one", 8000)?;
    recording.write(Frame::new(&[10; 160], 8000).map_err(StorageError::from)?)?;
    recording.finish()?;
    std::fs::create_dir(directory.path().join("sounds"))?;
    std::os::unix::fs::symlink(
        outside.path().join("recordings"),
        directory.path().join("sounds/digits"),
    )?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let sound = rusterisk_api::SoundId::try_from("digits/one".to_owned())
        .map_err(|_| WorkerError::Invalid)?;
    let result = port.play(PlaybackMedia::Sound(sound)).await;
    let cleanup = worker.close().await;
    assert!(matches!(
        result,
        Err(WorkerError::Storage(StorageError::InvalidName))
    ));
    cleanup
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
        port.play(PlaybackMedia::Sound(id("broken")?.into())).await,
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
    let result = port.play(PlaybackMedia::Sound(id("outside")?.into())).await;
    worker.close().await?;
    assert!(matches!(
        result,
        Err(WorkerError::Storage(StorageError::InvalidName))
    ));
    Ok(())
}
