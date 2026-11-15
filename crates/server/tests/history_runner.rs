use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_server::storage::StorageWorker;
use rusterisk_server::{ControlError, Controller, IncomingCall, IncomingTarget};
use rusterisk_storage::CallOutcome;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn config() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "[internal]\nexten => 100,1,Hangup()\n",
    )
    .map_err(failure)
}
fn incoming() -> Result<IncomingCall, ControlError> {
    Ok(IncomingCall {
        id: ChannelId::try_from("reused".to_owned())?,
        endpoint: "phone".into(),
        caller: "100".into(),
        context: "internal".into(),
        extension: "100".into(),
        target: IncomingTarget::Dialplan,
    })
}

#[tokio::test]
async fn short_calls_are_saved_without_stasis_and_reused_identifiers_do_not_collide_after_restart(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    for expected in 1..=2 {
        let (controller, runner, _effects) = Controller::new(config()?, 4);
        let runner = runner.with_storage(storage.clone())?;
        let task = tokio::spawn(runner.run());
        let result = controller.incoming(incoming()?).await;
        controller.shutdown();
        task.await.map_err(failure)??;
        let key = result?;
        assert_eq!(key.generation, 1);
        let records = storage.call_records(10).await.map_err(failure)?;
        assert_eq!(records.len(), expected);
        assert!(records.iter().all(|record| record.channel == "reused"
            && record.generation == 1
            && record.outcome == CallOutcome::Completed));
        if records.len() == 2 {
            assert_ne!(records[0].id, records[1].id);
        }
    }
    worker.close().await.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn unavailable_history_rejects_admission_and_returns_the_worker_failure(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config()?, 4);
    let runner = runner.with_storage(storage)?;
    worker.close().await.map_err(failure)?;
    let task = tokio::spawn(runner.run());
    let result = controller.incoming(incoming()?).await;
    controller.shutdown();
    assert!(matches!(result, Err(ControlError::Adapter(_))));
    assert!(task.await.map_err(failure)?.is_err());
    Ok(())
}
