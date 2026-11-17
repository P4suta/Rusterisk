use super::super::{dialplan::DialplanFrame, CallSettings, Control, ControlError};
use crate::call_profiles::{BoundNativeProfiles, NativeTransportBinding};
use crate::storage::{PreparedMusic, StorageWorker};
use crate::{Action, CallKey, Effect, MediaOwner, Milestone, Outcome, Signal};
use rusterisk_api::{ChannelId, EndpointState};
use rusterisk_config::{Codec, Config, Dialplan, WebSocketConfig};
use rusterisk_core::{ContactPresence, SettingsCandidate, SettingsGeneration, SettingsLifetimes};
use std::path::Path;

#[derive(Clone, Copy)]
enum Profile {
    Starting,
    Replacement,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Starting => "phone",
            Self::Replacement => "replacement",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Replacement => "Replacement",
        }
    }

    fn port(self) -> u16 {
        match self {
            Self::Starting => 5061,
            Self::Replacement => 5062,
        }
    }
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn verify(condition: bool, reason: &'static str) -> Result<(), ControlError> {
    condition.then_some(()).ok_or(ControlError::Invalid(reason))
}

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn configuration(profile: Profile) -> Result<Config, ControlError> {
    let name = profile.name();
    let codec = match profile {
        Profile::Starting => "ulaw",
        Profile::Replacement => "alaw",
    };
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &format!("[general]\nallowed_origins=https://{name}.example\n[app]\ntype=user\npassword={name}\n"),
        &format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername={name}\npassword={name}\n[route]\ntype=aor\ncontact=sip:{name}@127.0.0.1:{}\n[{name}]\ntype=endpoint\ntransport=udp\nauth=auth\naors=route\nallow={codec}\ncontext={name}\n[shared-auth]\ntype=auth\nusername=shared\npassword=shared\n[shared]\ntype=aor\nmax_contacts=1\n[shared]\ntype=endpoint\ntransport=udp\nauth=shared-auth\naors=shared\nallow=ulaw\n", profile.port()),
        "",
        &format!("[{name}]\nexten => 100,1,Set(PROFILE={name})\n"),
    )
    .map_err(failure)
}

fn configured(capacity: usize) -> Result<Control, ControlError> {
    let config = configuration(Profile::Starting)?;
    initialize(&config, capacity)
}

fn initialize(config: &Config, capacity: usize) -> Result<Control, ControlError> {
    let mut control = Control::with_configuration(capacity, config);
    control.subscribe("app")?;
    let key = control.endpoint_key("shared")?;
    control.signal(
        Signal::EndpointContact {
            key,
            presence: ContactPresence::Present,
        },
        "observed",
    )?;
    Ok(control)
}

fn music_configuration(profile: Profile) -> Result<Config, ControlError> {
    configuration(profile)?
        .with_music_on_hold_configuration(&format!(
            "[{}]\nmode=files\ndirectory=music\n",
            profile.class()
        ))
        .map_err(failure)
}

fn prepare(
    control: &mut Control,
    profile: Profile,
    music: Option<PreparedMusic>,
) -> Result<SettingsCandidate, ControlError> {
    let config = match &music {
        Some(_) => music_configuration(profile)?,
        None => configuration(profile)?,
    };
    let mut settings = CallSettings::configured(&config);
    settings.music_catalog = music;
    control.prepare_configuration(config, settings)
}

fn originate(
    control: &mut Control,
    name: &str,
    profile: Profile,
) -> Result<(CallKey, SettingsGeneration), ControlError> {
    let outcome = control.apply(
        Action::Originate {
            id: id(name)?,
            endpoint: format!("PJSIP/{}", profile.name()),
            app: "app".into(),
            args: Vec::new(),
            caller: "displayed-caller".into(),
            timeout: 30,
        },
        "created",
        0,
    )?;
    let [Effect::Start { key, settings, .. }] = outcome.effects.as_slice() else {
        return Err(ControlError::Invalid("Expected one queued native profile"));
    };
    Ok((key.clone(), *settings))
}

#[derive(Debug, PartialEq)]
struct ControlState {
    generation: u64,
    settings: SettingsGeneration,
    endpoint: super::EndpointKey,
    channels: serde_json::Value,
    bridges: serde_json::Value,
    endpoints: serde_json::Value,
    applications: serde_json::Value,
    dialplan: Dialplan,
    websocket: WebSocketConfig,
}

fn body(control: &mut Control, action: Action) -> Result<serde_json::Value, ControlError> {
    let outcome = control.apply(action, "observed", 0)?;
    assert!(outcome.effects.is_empty());
    outcome
        .body
        .ok_or(ControlError::Invalid("Expected a control query result"))
}

fn state(control: &mut Control) -> Result<ControlState, ControlError> {
    Ok(ControlState {
        generation: control.generation,
        settings: control.settings.current(),
        endpoint: control.endpoint_key("shared")?,
        channels: body(control, Action::ListChannels)?,
        bridges: body(control, Action::ListBridges)?,
        endpoints: body(control, Action::ListEndpoints { technology: None })?,
        applications: body(control, Action::ListApplications)?,
        dialplan: control.dialplan_program.clone(),
        websocket: control.websocket.clone(),
    })
}

fn native_profile(
    control: &mut Control,
    key: &CallKey,
    generation: SettingsGeneration,
) -> Result<(u16, Vec<Codec>), ControlError> {
    let config = configuration(Profile::Starting)?;
    let requested = config
        .transports
        .first()
        .cloned()
        .ok_or(ControlError::Capacity)?;
    let address = requested.bind;
    let binding = NativeTransportBinding::new(requested, address, 0, None);
    let bindings = BoundNativeProfiles::new([binding].into_iter().collect());
    let captured = control
        .take_native_profile(key, generation)?
        .into_profile()?
        .bind(&bindings)?;
    let engine = rusterisk_sip::Engine::new(config, [7; 32]);
    let prepared = engine.prepare_outbound(captured.sip, 0).map_err(failure)?;
    let rusterisk_sip::Destination::Static(target) = prepared.destination() else {
        return Err(ControlError::Invalid("Expected a static starting route"));
    };
    let port = target.port().ok_or(ControlError::Capacity)?;
    Ok((port, captured.media.codecs))
}

fn program<'a>(control: &'a Control, key: &CallKey) -> Result<&'a Dialplan, ControlError> {
    control
        .channels
        .get(&key.id)
        .ok_or(ControlError::Capacity)?
        .dialplan
        .as_ref()
        .map(DialplanFrame::program)
        .ok_or(ControlError::Capacity)
}

fn shared_observation(
    control: &mut Control,
    previous: super::EndpointKey,
) -> Result<(), ControlError> {
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Online)
    );
    let current = control.endpoint_key("shared")?;
    assert_eq!(
        current.generation,
        previous
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?
    );
    control.signal(
        Signal::EndpointContact {
            key: previous,
            presence: ContactPresence::Absent,
        },
        "stale",
    )?;
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Online)
    );
    Ok(())
}

fn password(config: &Config) -> Result<&str, ControlError> {
    Ok(config
        .users
        .first()
        .ok_or(ControlError::Capacity)?
        .password
        .expose())
}

#[test]
fn control_preparation_and_cancellation_preserve_every_published_resource(
) -> Result<(), ControlError> {
    let mut control = configured(4)?;
    let (call, starting) = originate(&mut control, "queued", Profile::Starting)?;
    control.apply(
        Action::CreateBridge {
            id: id("bridge")?,
            name: "existing".into(),
        },
        "created",
        0,
    )?;
    let before = state(&mut control)?;
    let candidate = prepare(&mut control, Profile::Replacement, None)?;
    assert_eq!(state(&mut control)?, before);
    assert!(matches!(
        prepare(&mut control, Profile::Replacement, None),
        Err(ControlError::Capacity)
    ));
    control.cancel_configuration(candidate)?;
    assert_eq!(state(&mut control)?, before);
    assert_eq!(
        native_profile(&mut control, &call, starting)?,
        (5061, [Codec::Pcmu].into())
    );
    Ok(())
}

#[test]
fn control_commit_moves_new_defaults_without_recreating_live_observations(
) -> Result<(), ControlError> {
    let mut control = configured(4)?;
    let (old, starting) = originate(&mut control, "old", Profile::Starting)?;
    control.apply(
        Action::CreateBridge {
            id: id("bridge")?,
            name: "existing".into(),
        },
        "created",
        0,
    )?;
    let channel = control.channel(&old.id)?.clone();
    let bridge = control.bridge(&id("bridge")?)?.clone();
    let bridge_generation = control
        .bridges
        .get(&id("bridge")?)
        .ok_or(ControlError::Capacity)?
        .token;
    let observation = control.endpoint_key("shared")?;
    let prepared = prepare(&mut control, Profile::Replacement, None)?;
    let replacement = control.commit_configuration(prepared)?;
    assert_eq!(control.channel(&old.id)?, &channel);
    assert_eq!(control.bridge(&id("bridge")?)?, &bridge);
    assert_eq!(
        control
            .bridges
            .get(&id("bridge")?)
            .ok_or(ControlError::Capacity)?
            .token,
        bridge_generation
    );
    shared_observation(&mut control, observation)?;
    assert!(control.endpoint_key("phone").is_err());
    assert!(control.endpoint_key("replacement").is_ok());
    assert_eq!(control.dialplan_program, replacement.dialplan);
    assert_eq!(password(&replacement)?, "replacement");
    assert_eq!(replacement.origins, ["https://replacement.example"]);
    let (new, current) = originate(&mut control, "new", Profile::Replacement)?;
    assert_ne!(current, starting);
    assert_eq!(
        program(&control, &old)?
            .steps
            .first()
            .ok_or(ControlError::Capacity)?
            .context,
        "phone"
    );
    assert_eq!(program(&control, &new)?, &replacement.dialplan);
    assert_eq!(
        native_profile(&mut control, &old, starting)?,
        (5061, [Codec::Pcmu].into())
    );
    assert_eq!(
        native_profile(&mut control, &new, current)?,
        (5062, [Codec::Pcma].into())
    );
    Ok(())
}

#[test]
fn control_commit_preserves_contact_changes_received_after_preparation() -> Result<(), ControlError>
{
    let mut control = configured(4)?;
    let previous = control.endpoint_key("shared")?;
    let candidate = prepare(&mut control, Profile::Replacement, None)?;
    control.signal(
        Signal::EndpointContact {
            key: previous.clone(),
            presence: ContactPresence::Absent,
        },
        "updated after preparation",
    )?;
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Offline)
    );
    control.commit_configuration(candidate)?;
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Offline)
    );
    control.signal(
        Signal::EndpointContact {
            key: previous,
            presence: ContactPresence::Present,
        },
        "stale after publication",
    )?;
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Offline)
    );
    let current = control.endpoint_key("shared")?;
    control.signal(
        Signal::EndpointContact {
            key: current,
            presence: ContactPresence::Present,
        },
        "current after publication",
    )?;
    assert_eq!(
        control.endpoint("PJSIP", "shared")?.state,
        Some(EndpointState::Online)
    );
    Ok(())
}

fn combine<T>(
    primary: Result<T, ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<T, ControlError> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; publication fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn wave(root: &Path) -> Result<(), ControlError> {
    let mut recording =
        rusterisk_storage::Recorder::create(root, "music-stage", 8000).map_err(failure)?;
    recording
        .write(rusterisk_media::Frame::new(&[1234; 160], 8000).map_err(failure)?)
        .map_err(failure)?;
    std::fs::create_dir(root.join("music")).map_err(failure)?;
    std::fs::rename(
        recording.finish().map_err(failure)?,
        root.join("music/known.wav"),
    )
    .map_err(failure)
}

async fn music_fixture(
    check: impl FnOnce(&mut Control, PreparedMusic) -> Result<(), ControlError>,
) -> Result<(), ControlError> {
    let data = tempfile::tempdir().map_err(failure)?;
    let result = async {
        wave(data.path())?;
        let (port, worker) = StorageWorker::start(data.path().to_owned())
            .await
            .map_err(failure)?;
        let operation = async {
            let mut catalogs = Vec::with_capacity(2);
            for profile in [Profile::Starting, Profile::Replacement] {
                let config = music_configuration(profile)?;
                catalogs.push(
                    port.prepare_music(config.music_on_hold)
                        .await
                        .map_err(failure)?,
                );
            }
            let mut catalogs = catalogs.into_iter();
            let mut control = initialize(&music_configuration(Profile::Starting)?, 4)?
                .with_music(catalogs.next().ok_or(ControlError::Capacity)?)?;
            check(&mut control, catalogs.next().ok_or(ControlError::Capacity)?)
        }
        .await;
        combine(operation, worker.close().await.map_err(failure))
    }
    .await;
    combine(result, data.close().map_err(failure))
}

fn selected_music(
    outcome: &Outcome,
    class: &str,
) -> Result<rusterisk_core::MediaTaskToken, ControlError> {
    let [Effect::StartMusic {
        token,
        selection: Ok(selection),
        ..
    }] = outcome.effects.as_slice()
    else {
        return Err(ControlError::Invalid("Expected a prepared music selection"));
    };
    verify(
        selection.class_name() == class,
        "Starting music class changed",
    )?;
    Ok(*token)
}

#[tokio::test]
async fn control_publication_preserves_running_and_later_music_for_started_calls(
) -> Result<(), ControlError> {
    music_fixture(|control, catalog| {
        let (old, starting) = originate(control, "old-music", Profile::Starting)?;
        control.signal(
            Signal::Milestone {
                key: old.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
        )?;
        let start = control.apply(
            Action::StartMusic {
                channel: old.id.clone(),
                class: Some("Starting".into()),
            },
            "music",
            0,
        )?;
        let token = selected_music(&start, "Starting")?;
        let candidate = prepare(control, Profile::Replacement, Some(catalog))?;
        control.commit_configuration(candidate)?;
        verify(
            control.owns_music(&MediaOwner::Channel(old.clone()), token),
            "Publication stopped existing music",
        )?;
        verify(
            control.settings.get(starting).is_ok(),
            "Starting settings were released before the call ended",
        )?;
        control.apply(Action::StopMusic(old.id.clone()), "stopped", 1)?;
        let resumed = control.apply(
            Action::StartMusic {
                channel: old.id.clone(),
                class: Some("Starting".into()),
            },
            "later",
            2,
        )?;
        selected_music(&resumed, "Starting")?;
        let (new, current) = originate(control, "new-music", Profile::Replacement)?;
        verify(starting != current, "New call retained superseded settings")?;
        control.signal(
            Signal::Milestone {
                key: new.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
        )?;
        let current = control.apply(
            Action::StartMusic {
                channel: new.id.clone(),
                class: Some("Replacement".into()),
            },
            "replacement",
            3,
        )?;
        selected_music(&current, "Replacement")?;
        control.apply(Action::Hangup(old.id), "ended", 4)?;
        verify(
            control.settings.get(starting).is_err(),
            "Terminal call retained obsolete settings",
        )?;
        control.shutdown("cleanup")?;
        Ok(())
    })
    .await
}

#[test]
fn control_publication_rejects_settings_capacity_before_changing_any_resource(
) -> Result<(), ControlError> {
    let mut control = configured(4)?;
    let mut retained = Vec::with_capacity(SettingsLifetimes::CAPACITY);
    for ordinal in 0..SettingsLifetimes::CAPACITY {
        retained.push(control.settings.retain(control.settings.current())?);
        if ordinal + 1 < SettingsLifetimes::CAPACITY {
            let candidate = prepare(&mut control, Profile::Starting, None)?;
            control.commit_configuration(candidate)?;
        }
    }
    let before = state(&mut control)?;
    assert!(matches!(
        prepare(&mut control, Profile::Replacement, None),
        Err(ControlError::Capacity)
    ));
    assert_eq!(state(&mut control)?, before);
    let mut retained = retained.into_iter();
    control
        .settings
        .release(retained.next().ok_or(ControlError::Capacity)?)?;
    let candidate = prepare(&mut control, Profile::Replacement, None)?;
    control.commit_configuration(candidate)?;
    for reference in retained {
        control.settings.release(reference)?;
    }
    Ok(())
}

#[test]
fn control_publication_checks_endpoint_generation_before_reserving_settings(
) -> Result<(), ControlError> {
    let mut control = configured(4)?;
    let generation = control.endpoints.generation;
    control.endpoints.generation = u64::MAX;
    let before = state(&mut control)?;
    assert!(matches!(
        prepare(&mut control, Profile::Replacement, None),
        Err(ControlError::Capacity)
    ));
    assert_eq!(state(&mut control)?, before);
    control.endpoints.generation = generation;
    let candidate = prepare(&mut control, Profile::Replacement, None)?;
    control.commit_configuration(candidate)?;
    Ok(())
}

#[test]
fn control_publication_rejects_foreign_and_stale_candidates_without_losing_the_current_candidate(
) -> Result<(), ControlError> {
    let mut control = configured(4)?;
    let mut foreign = configured(4)?;
    let foreign_candidate = prepare(&mut foreign, Profile::Replacement, None)?;
    let stale = prepare(&mut control, Profile::Replacement, None)?;
    let before = state(&mut control)?;
    assert!(control.commit_configuration(foreign_candidate).is_err());
    assert!(control.cancel_configuration(foreign_candidate).is_err());
    assert_eq!(state(&mut control)?, before);
    control.cancel_configuration(stale)?;
    let current = prepare(&mut control, Profile::Replacement, None)?;
    assert!(control.commit_configuration(stale).is_err());
    assert!(control.cancel_configuration(stale).is_err());
    assert_eq!(state(&mut control)?, before);
    let moved = control.commit_configuration(current)?;
    assert_eq!(password(&moved)?, "replacement");
    let published = state(&mut control)?;
    assert!(control.commit_configuration(current).is_err());
    assert!(control.cancel_configuration(current).is_err());
    assert_eq!(state(&mut control)?, published);
    foreign.cancel_configuration(foreign_candidate)?;
    Ok(())
}
