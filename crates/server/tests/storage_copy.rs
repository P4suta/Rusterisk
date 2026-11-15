use rusterisk_api::ChannelId;
use rusterisk_media::Frame;
use rusterisk_server::storage::{StorageWorker, WorkerError};
use rusterisk_storage::StorageError;

fn id(name: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(name.to_owned()).map_err(|_| WorkerError::Invalid)
}

#[tokio::test]
async fn stored_recording_copies_publish_exact_bytes_and_keep_live_names_reserved(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let result = async {
        let mut recording = storage.record(id("source")?, 8000).await?;
        for _ in 0..100 {
            recording.try_write(Frame::new(&[1234; 160], 8000).map_err(StorageError::from)?)?;
            storage.recordings().await?;
        }
        recording.finish().await?;
        let expected = std::fs::read(directory.path().join("recordings/source.wav"))?;
        let pending = storage.record(id("pending")?, 8000).await?;
        assert_eq!(
            storage
                .copy_recording(id("source")?, id("copy")?)
                .await?
                .name,
            "copy"
        );
        assert_eq!(
            std::fs::read(directory.path().join("recordings/copy.wav"))?,
            expected
        );
        for destination in ["source", "copy", "pending"] {
            assert!(matches!(
                storage
                    .copy_recording(id("source")?, id(destination)?)
                    .await,
                Err(WorkerError::Storage(StorageError::Exists))
            ));
        }
        assert!(matches!(
            storage.copy_recording(id("missing")?, id("copy")?).await,
            Err(WorkerError::Storage(StorageError::NotFound))
        ));
        pending.cancel().await?;
        Ok(())
    }
    .await;
    worker.close().await?;
    result
}
