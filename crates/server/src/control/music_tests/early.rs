use super::{
    fixture, play, request, Action, CallKey, Control, ControlError, Effect, IncomingCall,
    IncomingTarget, MediaOwner, MediaTaskKind, MediaTaskToken, Milestone, Outcome,
    PlaybackCompletion, Signal,
};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_core::{
    AudioGate, CallState, MediaConnection, MediaState, MuteDirection, PeerMediaDirection,
};

fn incoming(control: &mut Control, channel: ChannelId) -> Result<CallKey, ControlError> {
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
        "ringing",
    )?;
    control.key(&channel)
}

fn ringing(control: &mut Control, answered: CallKey) -> Result<CallKey, ControlError> {
    control.apply(Action::Hangup(answered.id.clone()), "fixture reset", 0)?;
    let key = incoming(control, answered.id)?;
    assert_ne!(key.generation, answered.generation);
    Ok(key)
}

fn no_start(outcome: &Outcome) {
    assert!(!outcome.effects.iter().any(|effect| matches!(
        effect,
        Effect::PrepareEarlyMusic { .. } | Effect::StartMusic { .. } | Effect::Answer { .. }
    )));
}

fn unchanged_milestones(
    control: &Control,
    key: &CallKey,
    media: MediaState,
    outcome: &Outcome,
) -> Result<(), ControlError> {
    let entry = control
        .channels
        .get(&key.id)
        .ok_or(ControlError::Capacity)?;
    assert_eq!(entry.call.generation(), key.generation);
    assert_eq!(entry.call.state(), CallState::Ringing);
    assert_eq!(entry.snapshot.state, ChannelState::Ring);
    assert_eq!(entry.call.media(), media);
    assert_eq!(entry.call.media_connection(), MediaConnection::Pending);
    assert!(!outcome
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Answer { .. })));
    assert!(!outcome.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ChannelStateChange { channel } if channel.state == ChannelState::Up
    )));
    assert!(!outcome.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ChannelVarset { variable, value, .. }
            if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1"
    )));
    Ok(())
}

fn remaining_quota(
    control: &mut Control,
    key: &CallKey,
    available: usize,
) -> Result<(), ControlError> {
    let tasks = control.media_tasks(&MediaOwner::Channel(key.clone()))?;
    let mut reserved = [None; 8];
    for slot in reserved.iter_mut().take(available) {
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

fn prepare(
    control: &mut Control,
    key: &CallKey,
    class: Option<&str>,
) -> Result<MediaTaskToken, ControlError> {
    let requested = control.apply(request(key, class), "accepted early music", 0)?;
    assert_eq!(requested.status, 204);
    let [Effect::PrepareEarlyMusic { key: owner, token }] = requested.effects.as_slice() else {
        return Err(ControlError::Invalid(
            "Expected only owned early music preparation",
        ));
    };
    assert_eq!(owner, key);
    let token = *token;
    assert!(control
        .media_tasks(&MediaOwner::Channel(key.clone()))?
        .state(token)
        .is_some());
    assert_eq!(control.music_source_count(), 1);
    unchanged_milestones(control, key, MediaState::Waiting, &requested)?;
    remaining_quota(control, key, 7)?;
    Ok(token)
}

fn prepared(key: &CallKey, token: MediaTaskToken) -> Signal {
    Signal::EarlyMediaPrepared {
        key: key.clone(),
        token,
        result: Ok(()),
    }
}

fn published(
    control: &Control,
    key: &CallKey,
    token: MediaTaskToken,
    class: &str,
    expected_gate: AudioGate,
    outcome: &Outcome,
) -> Result<(), ControlError> {
    let [Effect::Gate {
        key: gate_owner,
        gate: AudioGate::Closed,
        generated_audio_gate,
    }, Effect::StartMusic {
        key: source_owner,
        token: source_token,
        selection: Ok(selection),
    }] = outcome.effects.as_slice()
    else {
        return Err(ControlError::Invalid(
            "Expected current gates before the selected music source",
        ));
    };
    assert_eq!(gate_owner, key);
    assert_eq!(source_owner, key);
    assert_eq!(*source_token, token);
    assert_eq!(*generated_audio_gate, expected_gate);
    assert_eq!(selection.class_name(), class);
    unchanged_milestones(control, key, MediaState::Early, outcome)
}

fn gate(outcome: &Outcome, key: &CallKey, expected: AudioGate) -> Result<(), ControlError> {
    let [Effect::Gate {
        key: owner,
        gate: AudioGate::Closed,
        generated_audio_gate,
    }] = outcome.effects.as_slice()
    else {
        return Err(ControlError::Invalid(
            "Expected an updated early generated audio gate",
        ));
    };
    assert_eq!(owner, key);
    assert_eq!(*generated_audio_gate, expected);
    Ok(())
}

#[tokio::test]
async fn prepared_music_publishes_the_retained_class_after_the_latest_controls(
) -> Result<(), ControlError> {
    fixture("Main", |control, answered| {
        let key = ringing(control, answered)?;
        let token = prepare(control, &key, Some("MAIN"))?;
        let duplicate = control.apply(request(&key, Some("Main")), "preparing duplicate", 0)?;
        assert_eq!(duplicate.status, 204);
        assert!(duplicate.effects.is_empty());
        let muted = control.apply(
            Action::Mute {
                channel: key.id.clone(),
                direction: MuteDirection::Outgoing,
            },
            "mute during preparation",
            0,
        )?;
        no_start(&muted);
        let directed = control.signal(
            Signal::PeerDirection {
                key: key.clone(),
                direction: PeerMediaDirection::ReceiveOnly,
            },
            "peer direction during preparation",
        )?;
        no_start(&directed);
        let ready = control.signal(prepared(&key, token), "prepared")?;
        published(control, &key, token, "Main", AudioGate::Closed, &ready)?;
        assert_eq!(
            control.channels[&key.id].call.peer_direction(),
            PeerMediaDirection::ReceiveOnly
        );
        let duplicate =
            control.signal(prepared(&key, token), "duplicate preparation completion")?;
        no_start(&duplicate);
        unchanged_milestones(control, &key, MediaState::Early, &duplicate)?;
        remaining_quota(control, &key, 7)
    })
    .await
}

#[tokio::test]
async fn early_mute_and_peer_direction_updates_republish_generated_permission_without_answering(
) -> Result<(), ControlError> {
    fixture("default", |control, answered| {
        let key = ringing(control, answered)?;
        let token = prepare(control, &key, None)?;
        let ready = control.signal(prepared(&key, token), "prepared")?;
        published(control, &key, token, "default", AudioGate::Open, &ready)?;
        for (mute, expected) in [(true, AudioGate::OutgoingMuted), (false, AudioGate::Open)] {
            let action = if mute {
                Action::Mute {
                    channel: key.id.clone(),
                    direction: MuteDirection::Outgoing,
                }
            } else {
                Action::Unmute {
                    channel: key.id.clone(),
                    direction: MuteDirection::Outgoing,
                }
            };
            let changed = control.apply(action, "early mute change", 0)?;
            gate(&changed, &key, expected)?;
            unchanged_milestones(control, &key, MediaState::Early, &changed)?;
        }
        let directed = control.signal(
            Signal::PeerDirection {
                key: key.clone(),
                direction: PeerMediaDirection::ReceiveOnly,
            },
            "early peer direction",
        )?;
        let [Effect::PeerDirection {
            key: owner,
            direction: PeerMediaDirection::ReceiveOnly,
            gate: AudioGate::Closed,
            generated_audio_gate: AudioGate::IncomingMuted,
        }] = directed.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Expected current early peer permissions",
            ));
        };
        assert_eq!(owner, &key);
        unchanged_milestones(control, &key, MediaState::Early, &directed)?;
        for (mute, expected) in [(true, AudioGate::Closed), (false, AudioGate::IncomingMuted)] {
            let action = if mute {
                Action::Mute {
                    channel: key.id.clone(),
                    direction: MuteDirection::Outgoing,
                }
            } else {
                Action::Unmute {
                    channel: key.id.clone(),
                    direction: MuteDirection::Outgoing,
                }
            };
            let changed = control.apply(action, "restricted early mute change", 0)?;
            gate(&changed, &key, expected)?;
            unchanged_milestones(control, &key, MediaState::Early, &changed)?;
        }
        remaining_quota(control, &key, 7)
    })
    .await
}

#[tokio::test]
async fn answering_early_music_opens_conversation_without_claiming_a_received_connection(
) -> Result<(), ControlError> {
    fixture("default", |control, answered| {
        let key = ringing(control, answered)?;
        let token = prepare(control, &key, None)?;
        let ready = control.signal(prepared(&key, token), "prepared")?;
        published(control, &key, token, "default", AudioGate::Open, &ready)?;
        let answered = control.signal(
            Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            },
            "answered",
        )?;
        let [Effect::Gate {
            key: owner,
            gate: AudioGate::Open,
            generated_audio_gate: AudioGate::Open,
        }] = answered.effects.as_slice()
        else {
            return Err(ControlError::Invalid(
                "Expected final Answer to restore conversation permission",
            ));
        };
        assert_eq!(owner, &key);
        let entry = control
            .channels
            .get(&key.id)
            .ok_or(ControlError::Capacity)?;
        assert_eq!(entry.call.generation(), key.generation);
        assert_eq!(entry.call.state(), CallState::Up);
        assert_eq!(entry.snapshot.state, ChannelState::Up);
        assert_eq!(entry.call.media(), MediaState::Active);
        assert_eq!(entry.call.media_connection(), MediaConnection::Pending);
        assert!(!answered.events.iter().any(|event| matches!(
            &event.kind,
            EventKind::ChannelVarset { variable, value, .. }
                if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1"
        )));
        remaining_quota(control, &key, 7)
    })
    .await
}

#[tokio::test]
async fn a_failed_preparation_releases_once_and_can_be_retried_without_old_token_effects(
) -> Result<(), ControlError> {
    fixture("default", |control, answered| {
        let key = ringing(control, answered)?;
        let failed = prepare(control, &key, None)?;
        for _ in 0..2 {
            let rejected = control.signal(
                Signal::EarlyMediaPrepared {
                    key: key.clone(),
                    token: failed,
                    result: Err(ControlError::Adapter(
                        "owned transport preparation failed".into(),
                    )),
                },
                "preparation failed",
            )?;
            no_start(&rejected);
            unchanged_milestones(control, &key, MediaState::Waiting, &rejected)?;
            assert_eq!(
                control
                    .media_tasks(&MediaOwner::Channel(key.clone()))?
                    .state(failed),
                None
            );
            assert_eq!(control.music_source_count(), 0);
            remaining_quota(control, &key, 8)?;
        }
        let retried = prepare(control, &key, None)?;
        assert_ne!(retried, failed);
        let stale = control.signal(prepared(&key, failed), "stale success after retry")?;
        no_start(&stale);
        unchanged_milestones(control, &key, MediaState::Waiting, &stale)?;
        remaining_quota(control, &key, 7)?;
        let ready = control.signal(prepared(&key, retried), "retry prepared")?;
        published(control, &key, retried, "default", AudioGate::Open, &ready)
    })
    .await
}

#[tokio::test]
async fn stopping_during_preparation_releases_quota_and_ignores_late_completion(
) -> Result<(), ControlError> {
    fixture("default", |control, answered| {
        let key = ringing(control, answered)?;
        let stopped = prepare(control, &key, None)?;
        let stop = control.apply(Action::StopMusic(key.id.clone()), "stop preparation", 0)?;
        assert_eq!(stop.status, 204);
        no_start(&stop);
        assert_eq!(control.music_source_count(), 0);
        assert_eq!(
            control
                .media_tasks(&MediaOwner::Channel(key.clone()))?
                .state(stopped),
            None
        );
        remaining_quota(control, &key, 8)?;
        let late = control.signal(prepared(&key, stopped), "stopped preparation completed")?;
        no_start(&late);
        unchanged_milestones(control, &key, MediaState::Waiting, &late)?;
        let restarted = prepare(control, &key, None)?;
        assert_ne!(restarted, stopped);
        let old = control.signal(prepared(&key, stopped), "old completion during restart")?;
        no_start(&old);
        remaining_quota(control, &key, 7)?;
        let ready = control.signal(prepared(&key, restarted), "restart prepared")?;
        published(control, &key, restarted, "default", AudioGate::Open, &ready)
    })
    .await
}

#[tokio::test]
async fn cancelled_preparation_cannot_admit_a_later_channel_generation() -> Result<(), ControlError>
{
    fixture("default", |control, answered| {
        let old = ringing(control, answered)?;
        let stopped = prepare(control, &old, None)?;
        let ended = control.signal(
            Signal::Terminated {
                key: old.clone(),
                cause: 16,
            },
            "early CANCEL",
        )?;
        no_start(&ended);
        assert!(!control.channels.contains_key(&old.id));
        assert_eq!(control.music_source_count(), 0);
        let late = control.signal(prepared(&old, stopped), "late cancelled preparation")?;
        no_start(&late);
        let current = incoming(control, old.id.clone())?;
        assert_ne!(current.generation, old.generation);
        let fresh = prepare(control, &current, None)?;
        let stale = control.signal(prepared(&old, stopped), "old generation completed")?;
        no_start(&stale);
        unchanged_milestones(control, &current, MediaState::Waiting, &stale)?;
        remaining_quota(control, &current, 7)?;
        let ready = control.signal(prepared(&current, fresh), "replacement prepared")?;
        published(control, &current, fresh, "default", AudioGate::Open, &ready)
    })
    .await
}

#[tokio::test]
async fn accepted_playback_interrupts_preparation_without_restarting_music_on_late_completion(
) -> Result<(), ControlError> {
    fixture("default", |control, answered| {
        let key = ringing(control, answered)?;
        let music = prepare(control, &key, None)?;
        let playback = control.apply(play(&key, "announcement")?, "interrupt preparation", 0)?;
        assert_eq!(playback.status, 201);
        no_start(&playback);
        let token = playback
            .effects
            .iter()
            .find_map(|effect| {
                if let Effect::PlayMedia {
                    owner: MediaOwner::Channel(owner),
                    token,
                    ..
                } = effect
                {
                    (owner == &key).then_some(*token)
                } else {
                    None
                }
            })
            .ok_or(ControlError::Invalid(
                "Expected the admitted channel playback",
            ))?;
        assert_ne!(token, music);
        assert_eq!(
            control
                .media_tasks(&MediaOwner::Channel(key.clone()))?
                .state(music),
            None
        );
        assert_eq!(control.music_source_count(), 1);
        remaining_quota(control, &key, 7)?;
        let late = control.signal(
            prepared(&key, music),
            "music preparation completed after playback",
        )?;
        no_start(&late);
        unchanged_milestones(control, &key, MediaState::Waiting, &late)?;
        remaining_quota(control, &key, 7)?;
        let finished = control.signal(
            Signal::MediaPlaybackFinished {
                owner: MediaOwner::Channel(key.clone()),
                token,
                completion: PlaybackCompletion::Completed,
            },
            "playback finished",
        )?;
        no_start(&finished);
        assert_eq!(control.music_source_count(), 0);
        remaining_quota(control, &key, 8)?;
        let late = control.signal(prepared(&key, music), "music not automatically resumed")?;
        no_start(&late);
        remaining_quota(control, &key, 8)
    })
    .await
}
