use super::CallSettings;
use crate::{Action, CallKey, Control, ControlError, Effect};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_core::SettingsGeneration;

fn configured() -> Result<Control, ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=test\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\nallow=ulaw\n",
        "",
        "",
    ).map_err(|error| ControlError::Adapter(error.to_string()))?;
    let mut control = Control::with_configuration(4, &config);
    control.subscribe("app")?;
    Ok(control)
}

fn start(control: &mut Control) -> Result<(CallKey, SettingsGeneration), ControlError> {
    let outcome = control.apply(
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
        return Err(ControlError::Invalid("Expected one native start key"));
    };
    Ok((key.clone(), *settings))
}

#[test]
fn a_native_profile_is_moved_once_without_changing_the_live_channel() -> Result<(), ControlError> {
    let mut control = configured()?;
    let (key, generation) = start(&mut control)?;
    assert!(control
        .take_native_profile(&key, generation)?
        .into_profile()
        .is_ok());
    assert!(control.take_native_profile(&key, generation).is_err());
    assert_eq!(control.key(&key.id)?, key);
    assert!(control.settings.get(generation).is_ok());
    Ok(())
}

#[test]
fn foreign_and_different_settings_keys_preserve_the_pending_capsule() -> Result<(), ControlError> {
    let mut control = configured()?;
    let foreign = configured()?;
    let (key, generation) = start(&mut control)?;
    assert!(control
        .take_native_profile(&key, foreign.settings.current())
        .is_err());
    let current = control.publish_call_settings(CallSettings::empty())?;
    assert_ne!(current, generation);
    assert!(control.take_native_profile(&key, current).is_err());
    assert!(control
        .take_native_profile(&key, generation)?
        .into_profile()
        .is_ok());
    assert_eq!(control.key(&key.id)?, key);
    Ok(())
}

#[test]
fn a_terminal_call_releases_its_settings_and_cannot_claim_a_reused_identifier(
) -> Result<(), ControlError> {
    let mut control = configured()?;
    let (previous, generation) = start(&mut control)?;
    let current = control.settings.get(generation)?.clone();
    let published = control.publish_call_settings(current)?;
    assert!(control.settings.get(generation).is_ok());
    control.apply(Action::Hangup(previous.id.clone()), "ended", 1)?;
    assert!(control.settings.get(generation).is_err());
    let (replacement, settings) = start(&mut control)?;
    assert_ne!(previous.generation, replacement.generation);
    assert_eq!(settings, published);
    assert!(control.take_native_profile(&previous, generation).is_err());
    assert!(control
        .take_native_profile(&replacement, settings)?
        .into_profile()
        .is_ok());
    Ok(())
}

#[test]
fn startup_identity_is_shared_with_the_settings_domain() -> Result<(), ControlError> {
    let control = configured()?;
    let other = configured()?;
    assert_ne!(
        control.server_info().settings_owner(),
        other.server_info().settings_owner()
    );
    assert_ne!(control.settings.current(), other.settings.current());
    Ok(())
}

#[test]
fn dial_children_and_their_parent_release_the_starting_settings_together(
) -> Result<(), ControlError> {
    let mut control = configured()?;
    let id = ChannelId::try_from("parent".to_owned())?;
    control.incoming(
        crate::IncomingCall {
            id: id.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: crate::IncomingTarget::Dialplan,
        },
        "incoming",
    )?;
    let parent = control.key(&id)?;
    let starting = control.settings.current();
    let program = rusterisk_config::Dialplan::parse(
        "[internal]\nexten => 100,1,Dial(PJSIP/phone&PJSIP/phone,5)\n",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let started = control.start_dialplan(&parent, program, 0, "start")?;
    assert_eq!(started.effects.len(), 2);
    let current = control.settings.get(starting)?.clone();
    control.publish_call_settings(current)?;
    assert!(control.settings.get(starting).is_ok());
    control.apply(Action::Hangup(id), "end", 1)?;
    assert!(control.channels.is_empty());
    assert!(control.settings.get(starting).is_err());
    Ok(())
}

#[test]
fn an_external_channel_releases_its_starting_settings_at_terminal_removal(
) -> Result<(), ControlError> {
    let mut control = configured()?;
    let id = ChannelId::try_from("external".to_owned())?;
    let target = rusterisk_api::ExternalRtpTarget::new(
        "127.0.0.1:12000"
            .parse::<std::net::SocketAddr>()
            .map_err(|error| ControlError::Adapter(error.to_string()))?,
        rusterisk_api::StaticRtpCodec::Pcmu,
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let starting = control.settings.current();
    control.apply(
        Action::ExternalRtp {
            id: id.clone(),
            app: "app".into(),
            target,
            data: String::new(),
        },
        "created",
        0,
    )?;
    let current = control.settings.get(starting)?.clone();
    control.publish_call_settings(current)?;
    assert!(control.settings.get(starting).is_ok());
    control.apply(Action::Hangup(id), "end", 1)?;
    assert!(control.settings.get(starting).is_err());
    Ok(())
}

#[test]
fn effect_storage_has_a_fixed_small_payload_budget() {
    assert!(core::mem::size_of::<Effect>() <= 256);
}
