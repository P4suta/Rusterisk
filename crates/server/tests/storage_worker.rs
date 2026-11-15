use rusterisk_api::ChannelId;
use rusterisk_server::storage::{CallOutcome, StorageWorker, WorkerError};

fn id(value: &str) -> Result<ChannelId, WorkerError> {
    ChannelId::try_from(value.to_owned()).map_err(|_| WorkerError::Invalid)
}

#[tokio::test]
async fn owned_storage_commands_survive_restart_and_close_with_live_client_handles(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir().map_err(WorkerError::from)?;
    let (port, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    port.start_call(id("complete")?, "start").await?;
    port.finish_call(id("complete")?, CallOutcome::Completed, "end")
        .await?;
    port.start_call(id("interrupted")?, "start").await?;
    assert_eq!(
        port.call_outcome(id("complete")?).await?,
        CallOutcome::Completed
    );
    assert_eq!(
        port.call_outcome(id("interrupted")?).await?,
        CallOutcome::Active
    );
    worker.close().await?;
    assert!(matches!(
        port.call_outcome(id("complete")?).await,
        Err(WorkerError::Closed)
    ));
    let (restarted, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    assert_eq!(
        restarted.call_outcome(id("complete")?).await?,
        CallOutcome::Completed
    );
    assert_eq!(
        restarted.call_outcome(id("interrupted")?).await?,
        CallOutcome::Interrupted
    );
    worker.close().await?;
    Ok(())
}

#[tokio::test]
async fn startup_errors_and_invalid_commands_do_not_strand_the_worker() -> Result<(), WorkerError> {
    let directory = tempfile::tempdir().map_err(WorkerError::from)?;
    let file = directory.path().join("file");
    std::fs::write(&file, "occupied").map_err(WorkerError::from)?;
    assert!(matches!(
        StorageWorker::start(file).await,
        Err(WorkerError::Storage(_))
    ));
    let (port, worker) = StorageWorker::start(directory.path().join("store")).await?;
    assert!(matches!(
        port.start_call(id("invalid")?, &"x".repeat(65)).await,
        Err(WorkerError::Invalid)
    ));
    port.start_call(id("kept")?, "start").await?;
    assert!(matches!(
        port.finish_call(id("kept")?, CallOutcome::Active, "end")
            .await,
        Err(WorkerError::Invalid)
    ));
    assert_eq!(port.call_outcome(id("kept")?).await?, CallOutcome::Active);
    assert!(port.recordings().await?.is_empty());
    worker.close().await?;
    Ok(())
}
