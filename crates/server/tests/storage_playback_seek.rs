use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{PlaybackMedia, StoragePort, StorageWorker, WorkerError};
use rusterisk_storage::{Recorder, StorageError};

fn name() -> Result<ChannelId, WorkerError> {
    ChannelId::try_from("seek".to_owned()).map_err(|_| WorkerError::Invalid)
}

fn write(root: &std::path::Path, values: &[i16]) -> Result<(), WorkerError> {
    let mut recording = Recorder::create(root, "seek", 8000)?;
    for value in values {
        recording.write(Frame::new(&[*value; 160], 8000).map_err(StorageError::from)?)?;
    }
    recording.finish()?;
    Ok(())
}

fn combine(
    operation: Result<(), WorkerError>,
    cleanup: Result<(), WorkerError>,
) -> Result<(), WorkerError> {
    match (operation, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(WorkerError::Storage(StorageError::Io(
            std::io::Error::other(format!("{operation}; storage cleanup failed: {cleanup}")),
        ))),
    }
}

async fn open_source(
    port: &StoragePort,
) -> Result<rusterisk_server::storage::PlaybackSource, WorkerError> {
    let source = port.play(PlaybackMedia::Recording(name()?)).await?;
    port.recordings().await?;
    Ok(source)
}

#[tokio::test]
async fn seeking_discards_prefetched_audio_and_uses_the_requested_sample() -> Result<(), WorkerError>
{
    let root = tempfile::tempdir()?;
    write(root.path(), &[1000, 2000, 3000, 4000, 5000])?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let operation = async {
        let mut source = open_source(&port).await?;
        assert_eq!(source.rate(), 8000);
        assert_eq!(source.sample_count(), 800);
        source.seek_sample(3 * 160)?;
        assert_eq!(source.sample_position(), 480);
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[4000; 160]
        );
        assert_eq!(source.sample_position(), 640);
        source.seek_sample(0)?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[1000; 160]
        );
        source.seek_sample(799)?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[5000]
        );
        assert!(source.read().await?.is_none());
        assert!(matches!(source.seek_sample(0), Err(WorkerError::Closed)));
        Ok(())
    }
    .await;
    combine(operation, worker.close().await)
}

#[tokio::test]
async fn seeking_can_replace_a_prefetched_end_marker_and_clamps_to_the_recording(
) -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    write(root.path(), &[1000])?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let operation = async {
        let mut source = open_source(&port).await?;
        source.seek_sample(0)?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[1000; 160]
        );
        source.seek_sample(u64::MAX)?;
        assert_eq!(source.sample_position(), 160);
        source.seek_sample(0)?;
        assert_eq!(
            source.read().await?.ok_or(WorkerError::Invalid)?.samples(),
            &[1000; 160]
        );
        assert!(source.read().await?.is_none());
        drop(source);
        let mut replacement = open_source(&port).await?;
        assert_eq!(
            replacement
                .read()
                .await?
                .ok_or(WorkerError::Invalid)?
                .samples(),
            &[1000; 160]
        );
        Ok(())
    }
    .await;
    combine(operation, worker.close().await)
}

#[tokio::test]
async fn a_failed_seek_read_preserves_the_storage_error_and_closes_the_source(
) -> Result<(), WorkerError> {
    let root = tempfile::tempdir()?;
    write(root.path(), &[1000, 2000, 3000, 4000])?;
    let (port, worker) = StorageWorker::start(root.path().to_owned()).await?;
    let operation = async {
        let mut source = open_source(&port).await?;
        let file = std::fs::OpenOptions::new().write(true).open(root.path().join("recordings/seek.wav"))?;
        file.set_len(44)?;
        source.seek_sample(0)?;
        assert!(matches!(source.read().await, Err(WorkerError::Storage(StorageError::Io(error))) if error.kind() == std::io::ErrorKind::UnexpectedEof));
        assert!(matches!(source.seek_sample(0), Err(WorkerError::Closed)));
        Ok(())
    }.await;
    combine(operation, worker.close().await)
}
