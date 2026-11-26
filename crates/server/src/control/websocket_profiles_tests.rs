use super::{CallSettings, Control, ControlError};
use crate::signaling::PreparedWebSocketProfiles;
use crate::{Action, CallKey, Effect};
use rusterisk_api::{ChannelId, MediaDirection, MediaFormat};
use rusterisk_config::Config;
use rusterisk_core::SettingsGeneration;

#[path = "websocket_profiles_tests/unprepared.rs"]
mod unprepared;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn configuration(name: &str) -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?
    .with_websocket_configuration(
        "",
        &format!("[{name}]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://127.0.0.1:18080/{name}\nprotocols=media\nreconnect_attempts=1\n"),
    )
    .map_err(failure)
}

fn configured() -> Result<(Control, Config), ControlError> {
    let config = configuration("starting")?;
    let mut control = Control::with_configuration(4, &config);
    control.bind_websocket_profiles(PreparedWebSocketProfiles::new(&config.websocket.clients)?)?;
    control.subscribe("calls")?;
    Ok((control, config))
}

fn start(control: &mut Control) -> Result<(CallKey, SettingsGeneration), ControlError> {
    let outcome = control.apply(
        Action::ExternalWebSocketClient {
            id: ChannelId::try_from("custody".to_owned())?,
            app: "calls".into(),
            client: "starting".into(),
            format: MediaFormat::Pcmu,
            control_format: None,
            direction: MediaDirection::Both,
            auto_answer: true,
            data: String::new(),
        },
        "accepted",
        0,
    )?;
    let [Effect::ConnectWebSocket { key, settings, .. }] = outcome.effects.as_slice() else {
        return Err(ControlError::Invalid(
            "Expected one captured WebSocket start",
        ));
    };
    Ok((key.clone(), *settings))
}

#[test]
fn queued_websocket_profiles_survive_replacement_and_move_once() -> Result<(), ControlError> {
    let (mut control, _) = configured()?;
    let (key, generation) = start(&mut control)?;
    let replacement = configuration("replacement")?;
    let mut settings = CallSettings::configured(&replacement);
    settings.websocket_profiles = Some(PreparedWebSocketProfiles::new(
        &replacement.websocket.clients,
    )?);
    let candidate = control.prepare_configuration(replacement, settings)?;
    control.commit_configuration(candidate)?;
    let capsule = control.take_websocket_profile(&key, generation)?;
    assert!(capsule.into_profile().is_ok());
    assert!(control.take_websocket_profile(&key, generation).is_err());
    assert_eq!(control.key(&key.id)?, key);
    Ok(())
}

#[test]
fn wrong_websocket_keys_preserve_the_pending_start() -> Result<(), ControlError> {
    let (mut control, _) = configured()?;
    let (foreign, _) = configured()?;
    let (key, generation) = start(&mut control)?;
    assert!(control
        .take_websocket_profile(&key, foreign.settings.current())
        .is_err());
    let mut wrong_call = key.clone();
    wrong_call.generation = wrong_call
        .generation
        .checked_add(1)
        .ok_or(ControlError::Capacity)?;
    assert!(control
        .take_websocket_profile(&wrong_call, generation)
        .is_err());
    assert!(control
        .take_websocket_profile(&key, generation)?
        .into_profile()
        .is_ok());
    Ok(())
}

#[test]
fn startup_websocket_binding_is_idempotent_and_a_profile_leaves_once() -> Result<(), ControlError> {
    let (mut control, config) = configured()?;
    let current = control.settings.current();
    control.bind_websocket_profiles(PreparedWebSocketProfiles::new(&config.websocket.clients)?)?;
    assert_eq!(control.settings.current(), current);
    let (key, generation) = start(&mut control)?;
    assert!(control
        .take_websocket_profile(&key, generation)?
        .into_profile()
        .is_ok());
    assert!(control.take_websocket_profile(&key, generation).is_err());
    Ok(())
}
