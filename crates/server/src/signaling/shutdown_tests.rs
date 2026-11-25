use super::{cleanup::combine, failure, Signaling};
use crate::media::MediaHub;
use crate::{ControlError, Controller, Credentials};
use rusterisk_config::Config;
use rusterisk_test_support::Wait;
use std::time::Duration;

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "",
        "",
        "",
    )
    .map_err(failure)
}

// The composed task ends its signaling before it waits for the call decisions' settings loader.
#[tokio::test]
async fn shutdown_ends_signaling_and_refuses_requests_while_a_settings_loader_runs(
) -> Result<(), ControlError> {
    let config = configuration()?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, _hub, notices) = MediaHub::new(4);
    let mut signaling =
        Signaling::bind(config.clone(), controller.clone(), runner, media, notices).await?;
    let upgrades = signaling.websocket_port();
    let directory = tempfile::tempdir().map_err(failure)?;
    let (release, waiting) = std::sync::mpsc::sync_channel::<()>(1);
    let (started, observed) = tokio::sync::oneshot::channel();
    let (reply, caller) = tokio::sync::oneshot::channel();
    signaling
        .control_for_test()
        .begin_loading_for_shutdown_test(
            directory.path().to_owned(),
            reply,
            ControlError::Adapter("settings loader fixture".into()),
            move |_directory| {
                let _observed = started.send(());
                let _released = waiting.recv_timeout(Duration::from_secs(5));
                Ok(config)
            },
        );
    Wait::Ready
        .bounded(observed)
        .await
        .map_err(failure)?
        .map_err(failure)?;
    controller.shutdown();
    let running = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let closed = Wait::Ready
        .bounded(upgrades.closed())
        .await
        .map_err(|_| ControlError::Invalid("Signaling waited for the settings loader"));
    // A request that arrives while the call decisions wait for the loader fails at once.
    let credentials = Credentials::new("app".into(), "test".into())?;
    let refused = match Wait::Ready
        .bounded(controller.subscribe(credentials, None, "app".into()))
        .await
    {
        Ok(Err(_)) => Ok(()),
        Ok(Ok(_)) => Err(ControlError::Invalid("Stopping control accepted a request")),
        Err(_) => Err(ControlError::Invalid(
            "A request waited for the stopping control",
        )),
    };
    let waited = !running.is_finished();
    let result = combine(combine(closed, refused), release.send(()).map_err(failure));
    let ran = Wait::Ready
        .bounded(running)
        .await
        .map_err(failure)
        .and_then(|joined| joined.map_err(failure))
        .map(drop);
    let result = combine(result, ran);
    drop(caller);
    combine(result, directory.close().map_err(failure))?;
    assert!(waited, "the composed task ended before its settings loader");
    Ok(())
}
