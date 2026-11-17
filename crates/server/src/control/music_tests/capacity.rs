use super::{
    admit, combine, failure, id, play, request, token, Action, CallKey, Config, Control,
    ControlError, Effect, Frame, IncomingCall, IncomingTarget, MediaOwner, MediaTaskKind,
    MediaTaskState, MediaTaskToken, Milestone, PlaybackControl, Signal, StorageWorker,
};

#[tokio::test]
async fn ordinary_playback_reserves_shared_capacity_before_stopping_music(
) -> Result<(), ControlError> {
    fixture(|control, key| {
        let mut playing = fill_sources(control, &key)?;
        let original = playing[0].1;
        assert!(matches!(
            control.apply(play(&key, "announcement")?, "full playback", 0),
            Err(ControlError::Capacity)
        ));
        assert!(control.playbacks.is_empty());
        assert_active(control, &playing)?;
        assert_eq!(control.music_source_count(), 64);
        assert!(control
            .apply(request(&key, None), "original music retained", 0)?
            .effects
            .is_empty());
        assert_remaining_quota(control, &key)?;
        let (released, _) = playing.pop().ok_or(ControlError::Capacity)?;
        control.apply(Action::Hangup(released.id), "release shared slot", 0)?;
        assert_eq!(control.music_source_count(), 63);
        let accepted = control.apply(play(&key, "announcement")?, "playback admitted", 0)?;
        assert_eq!(accepted.status, 201);
        let [Effect::ControlPlayback {
            token: stopped,
            operation: PlaybackControl::Stop,
        }, Effect::PlayMedia {
            owner: MediaOwner::Channel(owner),
            token: fresh,
            ..
        }] = accepted.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Expected one music stop and one playback request",
            ));
        };
        assert_eq!(*stopped, original);
        assert_eq!(owner, &key);
        assert_ne!(*fresh, original);
        assert_eq!(control.channels[&key.id].media_tasks.state(original), None);
        assert_eq!(
            control.channels[&key.id].media_tasks.state(*fresh),
            Some(MediaTaskState::Active)
        );
        assert!(control.channels[&key.id].music.is_none());
        assert_eq!(control.playbacks.len(), 1);
        assert_eq!(control.music_source_count(), 63);
        playing.retain(|(owner, _)| owner != &key);
        assert_active(control, &playing)?;
        assert_remaining_quota(control, &key)
    })
    .await
}

#[tokio::test]
async fn replacing_music_at_the_shared_source_limit_preserves_the_original_source_and_quota(
) -> Result<(), ControlError> {
    fixture(|control, key| {
        let playing = fill_sources(control, &key)?;
        let original = playing[0].1;
        assert!(matches!(
            control.apply(request(&key, Some("Other")), "full replacement", 0),
            Err(ControlError::Capacity)
        ));
        assert_active(control, &playing)?;
        assert_eq!(control.music_source_count(), 64);
        assert!(control
            .apply(request(&key, None), "original class retained", 0)?
            .effects
            .is_empty());
        assert_remaining_quota(control, &key)?;
        let stopped = control.apply(Action::StopMusic(key.id.clone()), "original stop", 0)?;
        assert!(matches!(
            stopped.effects.as_slice(),
            [Effect::ControlPlayback {
                token,
                operation: PlaybackControl::Stop,
            }] if *token == original
        ));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn starting_the_current_class_is_idempotent_at_the_shared_source_limit(
) -> Result<(), ControlError> {
    fixture(|control, key| {
        let playing = fill_sources(control, &key)?;
        let duplicate = control.apply(request(&key, Some("DEFAULT")), "full duplicate", 0)?;
        assert_eq!(duplicate.status, 204);
        assert!(duplicate.effects.is_empty());
        assert_eq!(control.music_source_count(), 64);
        assert_active(control, &playing)?;
        assert_remaining_quota(control, &key)
    })
    .await
}

#[tokio::test]
async fn a_released_shared_source_slot_allows_a_different_class_to_replace_music(
) -> Result<(), ControlError> {
    fixture(|control, key| {
        let mut playing = fill_sources(control, &key)?;
        let original = playing[0].1;
        let (released, _) = playing.pop().ok_or(ControlError::Capacity)?;
        let ended = control.apply(Action::Hangup(released.id.clone()), "release slot", 0)?;
        assert!(ended
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == &released)));
        assert!(!control.channels.contains_key(&released.id));
        assert_eq!(control.music_source_count(), 63);
        let replacement = control.apply(request(&key, Some("Other")), "replace", 0)?;
        assert_eq!(replacement.status, 204);
        let [Effect::ControlPlayback {
            token: stopped,
            operation: PlaybackControl::Stop,
        }, Effect::StartMusic {
            key: owner,
            token: fresh,
            selection: Ok(selection),
        }] = replacement.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Expected one reserved music replacement",
            ));
        };
        assert_eq!(*stopped, original);
        assert_eq!(owner, &key);
        assert_ne!(*fresh, original);
        assert_eq!(selection.class_name(), "Other");
        assert_eq!(control.channels[&key.id].media_tasks.state(original), None);
        playing[0].1 = *fresh;
        assert_eq!(control.music_source_count(), 63);
        assert_active(control, &playing)?;
        assert_remaining_quota(control, &key)
    })
    .await
}

#[tokio::test]
async fn hold_reserves_source_and_core_capacity_before_replacing_current_music(
) -> Result<(), ControlError> {
    fixture(|control, key| {
        let mut playing = fill_sources(control, &key)?;
        control.apply(Action::StopMusic(key.id.clone()), "select Other", 0)?;
        let other = token(
            &control.apply(request(&key, Some("Other")), "Other music", 0)?,
            &key,
        )?;
        playing[0].1 = other;
        let before = control.channels[&key.id].call;
        assert_eq!(control.music_source_count(), 64);
        assert!(matches!(
            control.apply(Action::Hold(key.id.clone()), "source capacity", 0),
            Err(ControlError::Capacity)
        ));
        assert_eq!(control.channels[&key.id].call, before);
        assert_active(control, &playing)?;
        assert_remaining_quota(control, &key)?;
        let (released, _) = playing.pop().ok_or(ControlError::Capacity)?;
        control.apply(Action::Hangup(released.id), "release shared slot", 0)?;
        assert_eq!(control.music_source_count(), 63);
        let mut reserved = [None; 7];
        for slot in &mut reserved {
            *slot = Some(
                control
                    .channels
                    .get_mut(&key.id)
                    .ok_or(ControlError::Capacity)?
                    .media_tasks
                    .reserve(MediaTaskKind::Playback)?,
            );
        }
        assert!(matches!(
            control.apply(Action::Hold(key.id.clone()), "core quota", 0),
            Err(ControlError::Capacity)
        ));
        assert_eq!(control.channels[&key.id].call, before);
        assert_eq!(control.music_source_count(), 63);
        assert_active(control, &playing)?;
        let tasks = &mut control
            .channels
            .get_mut(&key.id)
            .ok_or(ControlError::Capacity)?
            .media_tasks;
        assert!(matches!(
            tasks.reserve(MediaTaskKind::Playback),
            Err(rusterisk_core::MediaTaskError::Capacity)
        ));
        for task in reserved.into_iter().flatten() {
            assert_eq!(tasks.state(task), Some(MediaTaskState::Queued));
            assert!(tasks.finish(task));
        }
        let held = control.apply(Action::Hold(key.id.clone()), "admitted default", 0)?;
        assert_eq!(held.status, 204);
        let [Effect::Gate {
            key: owner,
            gate: rusterisk_core::AudioGate::Closed,
            generated_audio_gate: rusterisk_core::AudioGate::Open,
        }, Effect::ControlPlayback {
            token: stopped,
            operation: PlaybackControl::Stop,
        }, Effect::StartMusic {
            key: source,
            token: fresh,
            selection: Ok(selection),
        }] = held.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Expected an atomic three-effect HOLD replacement",
            ));
        };
        assert_eq!(owner, &key);
        assert_eq!(source, &key);
        assert_eq!(*stopped, other);
        assert_ne!(*fresh, other);
        assert_eq!(selection.class_name(), "default");
        assert_eq!(control.channels[&key.id].media_tasks.state(other), None);
        playing[0].1 = *fresh;
        assert_eq!(control.music_source_count(), 63);
        assert_active(control, &playing)
    })
    .await
}

fn fill_sources(
    control: &mut Control,
    initial: &CallKey,
) -> Result<Vec<(CallKey, MediaTaskToken)>, ControlError> {
    let mut playing = Vec::with_capacity(64);
    let original = token(
        &control.apply(request(initial, None), "initial music", 0)?,
        initial,
    )?;
    playing.push((initial.clone(), original));
    for index in 1..64 {
        let channel = id(&format!("music-{index}"))?;
        control.incoming(
            IncomingCall {
                id: channel.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            },
            "incoming",
        )?;
        let key = control.key(&channel)?;
        control.signal(
            Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
        )?;
        let music = token(&control.apply(request(&key, None), "fill music", 0)?, &key)?;
        playing.push((key, music));
    }
    assert_eq!(playing.len(), 64);
    assert_eq!(control.music_source_count(), 64);
    assert_active(control, &playing)?;
    Ok(playing)
}

fn assert_active(
    control: &Control,
    playing: &[(CallKey, MediaTaskToken)],
) -> Result<(), ControlError> {
    for (key, token) in playing {
        let entry = control
            .channels
            .get(&key.id)
            .ok_or(ControlError::Capacity)?;
        assert_eq!(entry.call.generation(), key.generation);
        assert_eq!(
            entry.media_tasks.state(*token),
            Some(MediaTaskState::Active)
        );
        assert_eq!(entry.music.as_ref().map(|music| music.token), Some(*token));
    }
    Ok(())
}

fn assert_remaining_quota(control: &mut Control, key: &CallKey) -> Result<(), ControlError> {
    let tasks = &mut control
        .channels
        .get_mut(&key.id)
        .ok_or(ControlError::Capacity)?
        .media_tasks;
    let mut reserved = [None; 7];
    for slot in &mut reserved {
        *slot = Some(tasks.reserve(MediaTaskKind::Playback)?);
    }
    assert!(matches!(
        tasks.reserve(MediaTaskKind::Playback),
        Err(rusterisk_core::MediaTaskError::Capacity)
    ));
    for token in reserved.into_iter().flatten() {
        assert!(tasks.finish(token));
    }
    Ok(())
}

async fn fixture(
    check: impl FnOnce(&mut Control, CallKey) -> Result<(), ControlError>,
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = async {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "",
            "",
            "",
        )
        .and_then(|config| {
            config.with_music_on_hold_configuration(
                "[default]\nmode=files\ndirectory=music\n[Other]\nmode=files\ndirectory=music\n",
            )
        })
        .map_err(failure)?;
        let mut recording =
            rusterisk_storage::Recorder::create(directory.path(), "music-stage", 8000)
                .map_err(failure)?;
        recording
            .write(Frame::new(&[1234; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
        std::fs::create_dir(directory.path().join("music")).map_err(failure)?;
        std::fs::rename(
            recording.finish().map_err(failure)?,
            directory.path().join("music/known.wav"),
        )
        .map_err(failure)?;
        let (port, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let prepared = port.prepare_music(config.music_on_hold.clone()).await;
        let closed = worker.close().await.map_err(failure);
        let result = match prepared {
            Ok(catalog) => (|| {
                let mut control = Control::with_configuration(64, &config).with_music(catalog);
                control.subscribe("app")?;
                let key = admit(&mut control)?;
                let result = check(&mut control, key);
                let channels = control.channels.keys().cloned().collect::<Vec<_>>();
                let mut cleanup = Ok(());
                for channel in channels {
                    cleanup = combine(
                        cleanup,
                        control
                            .apply(Action::Hangup(channel), "cleanup", 0)
                            .map(|_| ()),
                    );
                }
                combine(result, cleanup)
            })(),
            Err(error) => Err(failure(error)),
        };
        combine(result, closed)
    }
    .await;
    combine(result, directory.close().map_err(failure))
}
