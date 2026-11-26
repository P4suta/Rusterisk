use rusterisk_api::{ChannelId, EventKind, RecordingState};
use rusterisk_config::Config;
use rusterisk_server::media::{MediaHub, RecordingEnd};
use rusterisk_server::signaling::Signaling;
use rusterisk_server::storage::StorageWorker;
use rusterisk_server::{Action, ControlError, Controller, Credentials};
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "password".into())
}
async fn event(
    events: &mut tokio::sync::mpsc::Receiver<rusterisk_api::Event>,
) -> Result<EventKind, ControlError> {
    Ok(tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .map_err(failure)?
        .ok_or(ControlError::Capacity)?
        .kind)
}

#[tokio::test]
async fn recording_jobs_report_file_conflicts_and_finish_active_files_before_shutdown(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    rusterisk_storage::Recorder::create(directory.path(), "taken", 8000)
        .map_err(failure)?
        .finish()
        .map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices)
        .await?
        .with_storage(storage.clone());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result: Result<(), ControlError> = async {
        let mut subscription = controller.subscribe(credentials()?, None, "app".into()).await?;
        controller.execute(credentials()?, None, Action::CreateBridge { id: id("mix")?, name: String::new() }).await?;
        assert!(matches!(event(&mut subscription.events).await?, EventKind::BridgeCreated { .. }));
        assert!(matches!(controller.execute(credentials()?, None, Action::RecordBridge { bridge: id("mix")?, name: id("taken")?, options: Default::default() }).await, Err(ControlError::Conflict(_))));
        assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFailed { recording } if recording.cause.as_deref() == Some("Recording already exists")));
        for name in ["saved", "shutdown"] {
            controller.execute(credentials()?, None, Action::RecordBridge { bridge: id("mix")?, name: id(name)?, options: Default::default() }).await?;
            assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingStarted { recording } if recording.name == name && recording.state == RecordingState::Recording));
            if name == "saved" {
                controller.execute(credentials()?, None, Action::EndRecording { name: id(name)?, mode: RecordingEnd::Finish }).await?;
                assert!(matches!(event(&mut subscription.events).await?, EventKind::RecordingFinished { recording } if recording.name == name && recording.state == RecordingState::Done));
                assert!(storage.recordings().await.map_err(failure)?.iter().any(|recording| recording.name == name));
            }
        }
        Ok(())
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    worker.close().await.map_err(failure)?;
    result?;
    let store = rusterisk_storage::Store::open(directory.path()).map_err(failure)?;
    assert_eq!(
        store
            .recordings()
            .map_err(failure)?
            .iter()
            .map(|recording| recording.name.as_str())
            .collect::<Vec<_>>(),
        ["saved", "shutdown", "taken"]
    );
    Ok(())
}
