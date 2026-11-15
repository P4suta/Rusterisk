use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{StorageWorker, WorkerError};
use rusterisk_storage::StorageError;

fn id(name: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(name.to_owned()).map_err(|_| WorkerError::Invalid)
}

#[tokio::test]
async fn stored_recording_queries_keep_pending_files_private_and_delete_only_completed_audio(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = async {
        let pending = storage.record(id("pending")?, 8000).await?;
        assert!(matches!(
            storage.recording(id("pending")?).await,
            Err(WorkerError::Storage(StorageError::NotFound))
        ));
        let mut finished = storage.record(id("saved")?, 8000).await?;
        finished.try_write(Frame::new(&[1234; 160], 8000).map_err(StorageError::from)?)?;
        finished.finish().await?;
        let details = storage.recording(id("saved")?).await?;
        assert_eq!(details.name, "saved");
        assert_eq!(details.format, "wav");
        storage.delete_recording(id("saved")?).await?;
        assert!(matches!(
            storage.recording(id("saved")?).await,
            Err(WorkerError::Storage(StorageError::NotFound))
        ));
        assert!(matches!(
            storage.delete_recording(id("saved")?).await,
            Err(WorkerError::Storage(StorageError::NotFound))
        ));
        assert!(storage.recordings().await?.is_empty());
        assert!(directory.path().join("recordings/pending.partial").exists());
        pending.cancel().await?;
        Ok(())
    }
    .await;
    worker.close().await?;
    result
}
