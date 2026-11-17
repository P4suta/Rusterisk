use super::{fixture, request, token};
use crate::{media::PlaybackControl, Action, CallKey, ControlError, Effect, Outcome};
use rusterisk_core::{AudioGate, MediaState, MediaTaskKind, MediaTaskState, MediaTaskToken};

fn default_music(outcome: &Outcome, key: &CallKey) -> Result<MediaTaskToken, ControlError> {
    let mut sources = outcome.effects.iter().filter_map(|effect| {
        if let Effect::StartMusic {
            key: actual,
            token,
            selection: Ok(selection),
        } = effect
        {
            (actual == key && selection.class_name() == "default").then_some(*token)
        } else {
            None
        }
    });
    let token = sources
        .next()
        .ok_or(ControlError::Invalid("HOLD did not request default music"))?;
    if sources.next().is_some() {
        return Err(ControlError::Invalid(
            "HOLD requested more than one music source",
        ));
    }
    Ok(token)
}

fn has_gate(outcome: &Outcome, key: &CallKey, voice: AudioGate, music: AudioGate) -> bool {
    outcome.effects.iter().any(|effect| {
        matches!(effect,
            Effect::Gate { key: actual, gate, generated_audio_gate }
                if actual == key && *gate == voice && *generated_audio_gate == music
        )
    })
}

fn stopped(outcome: &Outcome, expected: MediaTaskToken) -> bool {
    outcome.effects.iter().any(|effect| matches!(effect,
        Effect::ControlPlayback { token, operation: PlaybackControl::Stop } if *token == expected
    ))
}

#[tokio::test]
async fn hold_starts_the_default_source_while_closing_conversation_audio(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let held = control.apply(Action::Hold(key.id.clone()), "hold", 0)?;
        assert_eq!(held.status, 204);
        assert!(has_gate(&held, &key, AudioGate::Closed, AudioGate::Open));
        let music = default_music(&held, &key)?;
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Held);
        assert_eq!(
            control.channels[&key.id].media_tasks.state(music),
            Some(MediaTaskState::Active)
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn unhold_stops_the_current_music_and_restores_conversation_permissions(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let start = control.apply(request(&key, None), "music", 0)?;
        let music = token(&start, &key)?;
        control.apply(Action::Hold(key.id.clone()), "hold", 0)?;
        let resumed = control.apply(Action::Unhold(key.id.clone()), "unhold", 0)?;
        assert_eq!(resumed.status, 204);
        assert!(has_gate(&resumed, &key, AudioGate::Open, AudioGate::Open));
        assert!(stopped(&resumed, music));
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Active);
        assert_eq!(control.channels[&key.id].media_tasks.state(music), None);
        assert!(!resumed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::StartMusic { .. })));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn hold_without_a_default_class_is_accepted_without_starting_an_audio_source(
) -> Result<(), ControlError> {
    fixture("Main", |control, key| {
        let held = control.apply(Action::Hold(key.id.clone()), "hold without default", 0)?;
        assert_eq!(held.status, 204);
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Held);
        assert!(has_gate(&held, &key, AudioGate::Closed, AudioGate::Open));
        assert!(!held.effects.iter().any(|effect| matches!(
            effect,
            Effect::StartMusic {
                selection: Ok(_),
                ..
            } | Effect::PlayMedia { .. }
                | Effect::Playback { .. }
        )));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn unhold_stops_explicit_music_even_when_the_channel_is_already_active(
) -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let start = control.apply(request(&key, None), "explicit music", 0)?;
        let music = token(&start, &key)?;
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Active);
        let stopped_music = control.apply(Action::Unhold(key.id.clone()), "unhold active", 0)?;
        assert_eq!(stopped_music.status, 204);
        assert!(stopped(&stopped_music, music));
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Active);
        assert_eq!(control.channels[&key.id].call.audio_gate(), AudioGate::Open);
        assert_eq!(control.channels[&key.id].media_tasks.state(music), None);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn hold_rejects_a_full_media_quota_before_changing_the_call() -> Result<(), ControlError> {
    fixture("default", |control, key| {
        let before = control.channels[&key.id].call;
        let mut reserved = [None; 8];
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
            control.apply(Action::Hold(key.id.clone()), "full quota", 0),
            Err(ControlError::Capacity)
        ));
        assert_eq!(control.channels[&key.id].call, before);
        assert!(control.channels[&key.id].music.is_none());
        assert_eq!(control.music_source_count(), 0);
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
        let held = control.apply(Action::Hold(key.id.clone()), "quota released", 0)?;
        assert_eq!(held.status, 204);
        assert!(has_gate(&held, &key, AudioGate::Closed, AudioGate::Open));
        let music = default_music(&held, &key)?;
        assert_eq!(control.channels[&key.id].call.media(), MediaState::Held);
        assert_eq!(
            control.channels[&key.id].media_tasks.state(music),
            Some(MediaTaskState::Active)
        );
        Ok(())
    })
    .await
}
