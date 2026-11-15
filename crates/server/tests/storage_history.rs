use rusterisk_server::storage::{StorageWorker, WorkerError};
use rusterisk_storage::{CallHistoryChange, CallOutcome};

fn started() -> CallHistoryChange {
    CallHistoryChange::Started {
        id: "session-1".into(),
        channel: "phone".into(),
        generation: 1,
        timestamp: "start".into(),
    }
}

#[tokio::test]
async fn history_batches_are_queued_in_call_order_before_their_results_are_awaited(
) -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let start = storage.queue_history([started()].into_iter().collect())?;
    let finish = storage.queue_history(
        [CallHistoryChange::Finished {
            id: "session-1".into(),
            outcome: CallOutcome::Completed,
            timestamp: "end".into(),
        }]
        .into_iter()
        .collect(),
    )?;
    let records = storage.call_records(10).await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].channel, "phone");
    assert_eq!(records[0].generation, 1);
    assert_eq!(records[0].outcome, CallOutcome::Completed);
    finish.wait().await?;
    start.wait().await?;
    worker.close().await?;
    Ok(())
}

#[tokio::test]
async fn invalid_batches_are_rejected_before_storage_admission() -> Result<(), WorkerError> {
    let directory = tempfile::tempdir()?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned()).await?;
    let invalid = CallHistoryChange::Started {
        id: "../outside".into(),
        channel: "phone".into(),
        generation: 1,
        timestamp: "start".into(),
    };
    assert!(storage
        .queue_history([started(), invalid].into_iter().collect())
        .is_err());
    assert!(storage
        .queue_history((0..129).map(|_| started()).collect())
        .is_err());
    assert!(storage.call_records(10).await?.is_empty());
    worker.close().await?;
    assert!(matches!(
        storage.queue_history([started()].into_iter().collect()),
        Err(WorkerError::Closed)
    ));
    Ok(())
}
