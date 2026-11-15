use rusterisk_api::{ChannelId, RecordingIfExists};
use rusterisk_media::Frame;
use rusterisk_server::storage::{PlaybackMedia, StorageWorker, WorkerError};

fn id(value: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(value.to_owned()).map_err(|_| WorkerError::Invalid)
}
fn frame(sample: i16) -> Result<Frame, WorkerError> {
    Frame::new(&[sample; 160], 8000).map_err(|error| WorkerError::Storage(error.into()))
}

#[tokio::test]
async fn recording_replacements_keep_existing_playback_until_finish_and_cancel_without_data_loss(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = async {
        let mut original = storage.record(id("call")?, 8000).await?;
        original.try_write(frame(1234)?)?;
        original.finish().await?;
        let mut replacement = storage
            .record_with_policy(id("call")?, 8000, RecordingIfExists::Overwrite)
            .await?;
        replacement.try_write(frame(5678)?)?;
        let mut previous = storage.play(PlaybackMedia::Recording(id("call")?)).await?;
        assert_eq!(
            previous
                .read()
                .await?
                .ok_or(WorkerError::Invalid)?
                .samples(),
            [1234; 160]
        );
        replacement.cancel().await?;
        let mut replacement = storage
            .record_with_policy(id("call")?, 8000, RecordingIfExists::Overwrite)
            .await?;
        replacement.try_write(frame(7777)?)?;
        replacement.finish().await?;
        let mut current = storage.play(PlaybackMedia::Recording(id("call")?)).await?;
        assert_eq!(
            current.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            [7777; 160]
        );
        assert_eq!(storage.recordings().await?.len(), 1);
        let mut appended = storage
            .record_with_policy(id("call")?, 8000, RecordingIfExists::Append)
            .await?;
        appended.try_write(frame(8888)?)?;
        appended.finish().await?;
        let mut current = storage.play(PlaybackMedia::Recording(id("call")?)).await?;
        for expected in [7777, 8888] {
            assert_eq!(
                current.read().await?.ok_or(WorkerError::Invalid)?.samples(),
                [expected; 160]
            );
        }
        assert!(current.read().await?.is_none());
        let mut trimmed = storage.record(id("trimmed")?, 8000).await?;
        trimmed.try_write(frame(1234)?)?;
        trimmed.try_write(frame(0)?)?;
        trimmed.finish_with_tail_trim(10).await?;
        let mut current = storage
            .play(PlaybackMedia::Recording(id("trimmed")?))
            .await?;
        assert_eq!(
            current.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            [1234; 160]
        );
        assert_eq!(
            current.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            [0; 80]
        );
        assert!(current.read().await?.is_none());
        Ok(())
    }
    .await;
    worker.close().await?;
    result
}
