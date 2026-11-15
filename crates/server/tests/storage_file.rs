use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{StorageWorker, WorkerError};
use rusterisk_storage::StorageError;

fn id() -> Result<ChannelId, WorkerError> {
    ChannelId::try_from("saved".to_owned()).map_err(|_| WorkerError::Invalid)
}

async fn save(storage: &rusterisk_server::storage::StoragePort) -> Result<(), WorkerError> {
    let mut recording = storage.record(id()?, 8000).await?;
    for _ in 0..100 {
        recording.try_write(Frame::new(&[1234; 160], 8000).map_err(StorageError::from)?)?;
        storage.recordings().await?;
    }
    recording.finish().await
}

#[tokio::test]
async fn stored_file_readers_preserve_exact_bytes_after_deletion_and_release_bounded_slots_on_drop(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = async {
        save(&storage).await?;
        let expected = std::fs::read(directory.path().join("recordings/saved.wav"))?;
        let mut file = storage.recording_file(id()?).await?;
        assert_eq!(file.length(), expected.len() as u64);
        storage.delete_recording(id()?).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = file.read().await? {
            bytes.extend_from_slice(chunk.as_ref());
        }
        assert_eq!(bytes, expected);
        assert!(file.read().await?.is_none());
        assert!(matches!(
            storage.recording_file(id()?).await,
            Err(WorkerError::Storage(StorageError::NotFound))
        ));
        save(&storage).await?;
        let mut readers = Vec::new();
        for _ in 0..16 {
            readers.push(storage.recording_file(id()?).await?);
        }
        assert!(matches!(
            storage.recording_file(id()?).await,
            Err(WorkerError::Storage(StorageError::Capacity))
        ));
        drop(readers.pop());
        let replacement = storage.recording_file(id()?).await?;
        drop(replacement);
        drop(readers);
        Ok(())
    }
    .await;
    worker.close().await?;
    result
}
