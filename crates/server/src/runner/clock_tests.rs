use crate::{Action, ControlError, Controller, Credentials, Effect};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use std::time::Duration;
use tokio::time::Instant;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n",
        "",
        "",
    )
    .map_err(failure)
}

#[tokio::test(start_paused = true)]
async fn an_originated_channel_ends_at_its_timeout_on_virtual_time() -> Result<(), ControlError> {
    let (controller, mut runner, mut effects) = Controller::new(configuration()?, 4);
    runner.control.subscribe("app")?;
    let task = tokio::spawn(runner.run());
    let begun = Instant::now();
    controller
        .execute(
            Credentials::new("app".into(), "test".into())?,
            None,
            Action::Originate {
                id: ChannelId::try_from("timed".to_owned())?,
                endpoint: "PJSIP/phone".into(),
                app: "app".into(),
                args: Vec::new(),
                caller: "100".into(),
                timeout: 1,
            },
        )
        .await?;
    let Some(Effect::Start { key, .. }) = effects.recv().await else {
        return Err(ControlError::Invalid("Expected the call to start"));
    };
    let released = rusterisk_test_support::Wait::Ready
        .bounded(effects.recv())
        .await;
    let waited = begun.elapsed();
    controller.shutdown();
    task.await.map_err(failure)??;
    assert!(
        matches!(released, Ok(Some(Effect::Release { key: ended })) if ended == key),
        "the originate timeout released no call within five virtual seconds"
    );
    assert!(
        waited >= Duration::from_secs(1) && waited <= Duration::from_millis(1100),
        "the call ended after {waited:?} instead of at its one second timeout"
    );
    Ok(())
}
