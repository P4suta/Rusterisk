use crate::{Action, CallKey, ControlError, Controller, Effect};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::SettingsGeneration;
use tokio::sync::oneshot;

fn setup() -> Result<(Controller, super::Runner, CallKey, SettingsGeneration), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n",
        "",
        "",
    ).map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, mut runner, _effects) = Controller::new(config, 4);
    runner.control.subscribe("app")?;
    let outcome = runner.control.apply(
        Action::Originate {
            id: ChannelId::try_from("custody".to_owned())?,
            endpoint: "PJSIP/phone".into(),
            app: "app".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "accepted",
        0,
    )?;
    let [Effect::Start { key, settings, .. }] = outcome.effects.as_slice() else {
        return Err(ControlError::Invalid("Expected native custody"));
    };
    Ok((controller, runner, key.clone(), *settings))
}

#[tokio::test]
async fn a_cancelled_request_does_not_consume_the_native_profile() -> Result<(), ControlError> {
    let (_controller, mut runner, key, settings) = setup()?;
    let (reply, response) = oneshot::channel();
    drop(response);
    runner.native_profile_request(key.clone(), settings, reply)?;
    assert!(runner
        .control
        .take_native_profile(&key, settings)?
        .into_profile()
        .is_ok());
    Ok(())
}

#[tokio::test]
async fn a_failed_reply_restores_the_exact_unclaimed_native_capsule() -> Result<(), ControlError> {
    let (_controller, mut runner, key, settings) = setup()?;
    let capsule = runner.control.take_native_profile(&key, settings)?;
    let (reply, response) = oneshot::channel();
    drop(response);
    runner.deliver_native_profile(key.clone(), settings, Ok(capsule), reply)?;
    let (reply, mut response) = oneshot::channel();
    runner.native_profile_request(key.clone(), settings, reply)?;
    let capsule = response
        .try_recv()
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    assert!(capsule.into_profile().is_ok());
    assert!(runner.control.take_native_profile(&key, settings).is_err());
    assert_eq!(runner.control.key(&key.id)?, key);
    Ok(())
}

#[tokio::test]
async fn shutdown_cancels_a_waiting_handoff_without_waiting_for_effect_delivery(
) -> Result<(), ControlError> {
    let (controller, mut runner, key, settings) = setup()?;
    let claim = controller.take_native_profile(key.clone(), settings);
    let mut claim = std::pin::pin!(claim);
    let queued = tokio::select! {
        result = &mut claim => return Err(ControlError::Adapter(format!("Native handoff completed without control: {}", result.is_ok()))),
        request = runner.requests.recv() => request.ok_or(ControlError::Capacity)?,
    };
    controller.shutdown();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(1), claim)
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))?;
    assert!(cancelled.is_err());
    runner.request(queued, 0)?;
    assert!(runner
        .control
        .take_native_profile(&key, settings)?
        .into_profile()
        .is_ok());
    Ok(())
}
