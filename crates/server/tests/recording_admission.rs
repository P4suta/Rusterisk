use rusterisk_api::{ChannelId, EventKind, RecordingState};
use rusterisk_config::Config;
use rusterisk_server::media::RecordingEnd;
use rusterisk_server::{Action, ControlError, Controller, Credentials, Effect, Signal};
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

#[tokio::test]
async fn recording_replies_wait_for_admission_while_calls_and_cancellation_keep_progressing(
) -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, mut effects) = Controller::new(config, 4);
    let runner = tokio::spawn(runner.run());
    let result: Result<(), ControlError> = async {
        let mut subscription = controller.subscribe(credentials()?, None, "app".into()).await?;
        controller.execute(credentials()?, None, Action::CreateBridge { id: id("mix")?, name: String::new() }).await?;
        subscription.events.recv().await.ok_or(ControlError::Capacity)?;
        let client = controller.clone();
        let rejected = tokio::spawn(async move { client.execute(credentials()?, None, Action::RecordBridge { bridge: id("mix")?, name: id("taken")?, options: Default::default() }).await });
        let Effect::RecordMedia { owner, token, .. } = effects.recv().await.ok_or(ControlError::Capacity)? else { return Err(ControlError::Unsupported); };
        let fetched = controller.execute(credentials()?, None, Action::GetBridge(id("mix")?)).await?;
        assert_eq!(fetched.status, 200);
        assert!(!rejected.is_finished());
        controller.signal(Signal::RecordingAdmitted { owner, token, result: Err(ControlError::Conflict("Recording already exists")) }).await?;
        assert!(matches!(rejected.await.map_err(failure)?, Err(ControlError::Conflict(_))));
        assert!(matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::RecordingFailed { .. }));
        let client = controller.clone();
        let admitted = tokio::spawn(async move { client.execute(credentials()?, None, Action::RecordBridge { bridge: id("mix")?, name: id("saved")?, options: Default::default() }).await });
        let Effect::RecordMedia { owner, token, .. } = effects.recv().await.ok_or(ControlError::Capacity)? else { return Err(ControlError::Unsupported); };
        controller.signal(Signal::RecordingAdmitted { owner: owner.clone(), token, result: Ok(()) }).await?;
        assert_eq!(admitted.await.map_err(failure)??.status, 201);
        assert!(matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::RecordingStarted { .. }));
        controller.execute(credentials()?, None, Action::EndRecording { name: id("saved")?, mode: RecordingEnd::Cancel }).await?;
        effects.recv().await.ok_or(ControlError::Capacity)?;
        controller.signal(Signal::RecordingFinished { owner, token, result: Ok(()) }).await?;
        assert!(matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::RecordingFinished { recording } if recording.state == RecordingState::Canceled));
        let client = controller.clone();
        let abandoned = tokio::spawn(async move { client.execute(credentials()?, None, Action::RecordBridge { bridge: id("mix")?, name: id("abandoned")?, options: Default::default() }).await });
        let Effect::RecordMedia { owner, token, .. } = effects.recv().await.ok_or(ControlError::Capacity)? else { return Err(ControlError::Unsupported); };
        abandoned.abort();
        assert!(abandoned.await.is_err());
        let stopped = tokio::time::timeout(Duration::from_secs(1), effects.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
        assert!(matches!(stopped, Effect::EndRecording { token: current, mode: RecordingEnd::Cancel } if current == token));
        controller.signal(Signal::RecordingAdmitted { owner: owner.clone(), token, result: Ok(()) }).await?;
        controller.signal(Signal::RecordingFinished { owner, token, result: Ok(()) }).await?;
        assert!(matches!(subscription.events.recv().await.ok_or(ControlError::Capacity)?.kind, EventKind::RecordingFinished { recording } if recording.state == RecordingState::Canceled));
        Ok(())
    }.await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    result
}
