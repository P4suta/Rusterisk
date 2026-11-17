use super::{
    Action, CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, MediaOwner,
    Milestone, Outcome, PlaybackCompletion, Signal,
};
use crate::{media::PlaybackControl, storage::StorageWorker};
use rusterisk_api::{ChannelId, MediaUri};
use rusterisk_config::Config;
use rusterisk_core::{MediaTaskKind, MediaTaskState, MediaTaskToken};
use rusterisk_media::Frame;

mod capacity;
mod early;
mod hold;
mod settings;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn request(key: &CallKey, class: Option<&str>) -> Action {
    Action::StartMusic {
        channel: key.id.clone(),
        class: class.map(str::to_owned),
    }
}

fn token(outcome: &Outcome, key: &CallKey) -> Result<MediaTaskToken, ControlError> {
    match outcome.effects.as_slice() {
        [Effect::StartMusic {
            key: actual, token, ..
        }] if actual == key => Ok(*token),
        _ => Err(ControlError::Invalid("Expected one music source request")),
    }
}

#[tokio::test]
async fn music_start_and_stop_are_idempotent_and_preserve_the_selected_class_identity(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let start = control.apply(request(&key, None), "start", 0)?;
        assert_eq!(start.status, 204);
        let music = token(&start, &key)?;
        assert!(matches!(&start.effects[0], Effect::StartMusic { selection: Ok(selection), .. } if selection.class_name() == "default"));
        let duplicate = control.apply(request(&key, Some("DEFAULT")), "duplicate", 0)?;
        assert_eq!(duplicate.status, 204);
        assert!(duplicate.effects.is_empty());
        assert_eq!(control.channels[&key.id].media_tasks.state(music), Some(MediaTaskState::Active));
        let stop = control.apply(Action::StopMusic(key.id.clone()), "stop", 0)?;
        assert_eq!(stop.status, 204);
        assert!(matches!(stop.effects.as_slice(), [Effect::ControlPlayback { token, operation: PlaybackControl::Stop }] if *token == music));
        assert_eq!(control.channels[&key.id].media_tasks.state(music), None);
        assert!(control.apply(Action::StopMusic(key.id.clone()), "duplicate stop", 0)?.effects.is_empty());
        Ok(())
    }).await
}

#[tokio::test]
async fn an_unknown_requested_class_falls_back_to_the_configured_default(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let start = control.apply(request(&key, Some("missing")), "start", 0)?;
        assert_eq!(start.status, 204);
        token(&start, &key)?;
        assert!(matches!(&start.effects[0], Effect::StartMusic { selection: Ok(selection), .. } if selection.class_name() == "default"));
        assert!(control.apply(request(&key, Some("default")), "same selected class", 0)?.effects.is_empty());
        Ok(())
    }).await
}

#[tokio::test]
async fn unavailable_classes_are_accepted_then_release_only_the_failed_music_task(
) -> Result<(), ControlError> {
    fixture("Main", |control, key| {
        let missing = control.apply(request(&key, Some("missing")), "accepted", 0)?;
        assert_eq!(missing.status, 204);
        let failed = token(&missing, &key)?;
        assert!(matches!(&missing.effects[0], Effect::StartMusic { selection: Err(ControlError::Invalid(_)), .. }));
        let finished = control.signal(Signal::MediaPlaybackFinished {
            owner: MediaOwner::Channel(key.clone()), token: failed, completion: PlaybackCompletion::Failed,
        }, "failed")?;
        assert!(finished.effects.is_empty());
        assert_eq!(control.channels[&key.id].media_tasks.state(failed), None);
        let valid = control.apply(request(&key, Some("MAIN")), "valid", 0)?;
        let fresh = token(&valid, &key)?;
        assert_ne!(fresh, failed);
        assert!(matches!(&valid.effects[0], Effect::StartMusic { selection: Ok(selection), .. } if selection.class_name() == "Main"));
        Ok(())
    }).await
}

#[tokio::test]
async fn rejected_playback_preserves_music_and_its_shared_eight_slot_admission(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let music = token(&control.apply(request(&key, None), "music", 0)?, &key)?;
        for _ in 0..7 {
            control
                .channels
                .get_mut(&key.id)
                .ok_or(ControlError::Capacity)?
                .media_tasks
                .reserve(MediaTaskKind::Playback)?;
        }
        assert!(matches!(
            control.apply(play(&key, "announcement")?, "full", 0),
            Err(ControlError::Capacity)
        ));
        assert_eq!(
            control.channels[&key.id].media_tasks.state(music),
            Some(MediaTaskState::Active)
        );
        assert!(control.playbacks.is_empty());
        let duplicate = control.apply(request(&key, None), "still playing", 0)?;
        assert!(duplicate.effects.is_empty());
        let ended = control.apply(Action::Hangup(key.id.clone()), "ended", 0)?;
        assert!(ended
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key: released } if released == &key)));
        assert!(!control.channels.contains_key(&key.id));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn failed_class_lookup_preserves_active_music_and_its_existing_quota(
) -> Result<(), ControlError> {
    fixture("Main", |control, key| {
        let start = control.apply(request(&key, Some("Main")), "playing", 0)?;
        let original = token(&start, &key)?;
        let rejected = control.apply(request(&key, Some("missing")), "lookup failed", 0)?;
        assert_eq!(rejected.status, 204);
        assert!(!rejected.effects.iter().any(|effect| matches!(
            effect,
            Effect::ControlPlayback { operation: PlaybackControl::Stop, .. }
        )));
        assert_eq!(
            control.channels[&key.id].media_tasks.state(original),
            Some(MediaTaskState::Active)
        );
        let tasks = &mut control.channels.get_mut(&key.id).ok_or(ControlError::Capacity)?.media_tasks;
        let mut remaining = [None; 7];
        for slot in &mut remaining {
            *slot = Some(tasks.reserve(MediaTaskKind::Playback)?);
        }
        assert!(matches!(
            tasks.reserve(MediaTaskKind::Playback),
            Err(rusterisk_core::MediaTaskError::Capacity)
        ));
        for task in remaining.into_iter().flatten() {
            assert!(tasks.finish(task));
        }
        let stopped = control.apply(Action::StopMusic(key.id.clone()), "explicit stop", 0)?;
        assert!(matches!(
            stopped.effects.as_slice(),
            [Effect::ControlPlayback { token, operation: PlaybackControl::Stop }] if *token == original
        ));
        Ok(())
    }).await
}

#[tokio::test]
async fn accepted_playback_stops_music_after_reservation_and_never_restarts_it_implicitly(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let music = token(&control.apply(request(&key, None), "music", 0)?, &key)?;
        let playback = control.apply(play(&key, "announcement")?, "play", 0)?;
        let [Effect::ControlPlayback {
            token: stopped,
            operation: PlaybackControl::Stop,
        }, Effect::PlayMedia { token: playing, .. }] = playback.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Playback did not stop music before starting its reserved source",
            ));
        };
        assert_eq!(*stopped, music);
        assert_ne!(*playing, music);
        let playing = *playing;
        assert_eq!(control.channels[&key.id].media_tasks.state(music), None);
        assert_eq!(
            control.channels[&key.id].media_tasks.state(playing),
            Some(MediaTaskState::Active)
        );
        let finished = control.signal(
            Signal::MediaPlaybackFinished {
                owner: MediaOwner::Channel(key.clone()),
                token: playing,
                completion: PlaybackCompletion::Completed,
            },
            "finished",
        )?;
        assert!(finished.effects.is_empty());
        assert!(control
            .apply(Action::StopMusic(key.id.clone()), "not resumed", 0)?
            .effects
            .is_empty());
        let restarted = control.apply(request(&key, None), "explicit restart", 0)?;
        assert_ne!(token(&restarted, &key)?, music);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_previous_music_completion_cannot_stop_a_replacement_channel_generation(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let old = token(&control.apply(request(&key, None), "music", 0)?, &key)?;
        control.apply(Action::Hangup(key.id.clone()), "ended", 0)?;
        admit(control)?;
        let current = control.key(&key.id)?;
        assert_ne!(current.generation, key.generation);
        let fresh = token(
            &control.apply(request(&current, None), "replacement", 0)?,
            &current,
        )?;
        let stale = control.signal(
            Signal::MediaPlaybackFinished {
                owner: MediaOwner::Channel(key),
                token: old,
                completion: PlaybackCompletion::Completed,
            },
            "stale",
        )?;
        assert!(stale.effects.is_empty());
        assert!(stale.events.is_empty());
        assert_eq!(
            control.channels[&current.id].media_tasks.state(fresh),
            Some(MediaTaskState::Active)
        );
        Ok(())
    })
    .await
}

fn play(key: &CallKey, name: &str) -> Result<Action, ControlError> {
    Ok(Action::PlayChannel {
        channel: key.id.clone(),
        id: id(name)?,
        media: MediaUri::try_from("sound:known").map_err(failure)?,
        language: None,
        options: Default::default(),
    })
}

fn admit(control: &mut Control) -> Result<CallKey, ControlError> {
    control.incoming(
        IncomingCall {
            id: id("phone")?,
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
    let key = control.key(&id("phone")?)?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    Ok(key)
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; music control cleanup failed: {cleanup}"
        ))),
    }
}

async fn fixture(
    class: &str,
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
            config.with_music_on_hold_configuration(&format!(
                "[{class}]\nmode=files\ndirectory=music\n"
            ))
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
                let mut control = Control::with_configuration(4, &config).with_music(catalog)?;
                control.subscribe("app")?;
                let key = admit(&mut control)?;
                check(&mut control, key)
            })(),
            Err(error) => Err(failure(error)),
        };
        combine(result, closed)
    }
    .await;
    combine(result, directory.close().map_err(failure))
}
