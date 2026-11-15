use rusterisk_api::{ChannelId, EventKind, MediaUri};
use rusterisk_server::media::PlaybackControl;
use rusterisk_server::{
    Action, CallKey, Control, ControlError, Effect, Milestone, PlaybackCompletion, Signal,
};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}
fn media(value: &str) -> Result<MediaUri, ControlError> {
    MediaUri::try_from(value).map_err(|error| ControlError::Adapter(error.to_string()))
}

#[test]
fn empty_bridges_own_playback_and_deletion_cannot_affect_a_replacement_bridge(
) -> Result<(), ControlError> {
    use rusterisk_server::MediaOwner;
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let mut previous = None;
    for _ in 0..2 {
        control.apply(
            Action::CreateBridge {
                id: id("mix")?,
                name: String::new(),
            },
            "now",
            0,
        )?;
        let started = control.apply(
            Action::PlayBridge {
                bridge: id("mix")?,
                id: id("play")?,
                media: media("sound:welcome")?,
            },
            "now",
            0,
        )?;
        assert_eq!(
            started.body.ok_or(ControlError::Capacity)?["target_uri"],
            "bridge:mix"
        );
        let [Effect::PlayMedia { owner, token, .. }] = &started.effects[..] else {
            return Err(ControlError::Unsupported);
        };
        assert!(matches!(owner, MediaOwner::Bridge { id: bridge, .. } if bridge == &id("mix")?));
        let owner = owner.clone();
        let token = *token;
        if let Some((old_owner, old_token)) = previous {
            assert_ne!(old_owner, owner);
            assert!(control
                .signal(
                    Signal::MediaPlaybackFinished {
                        owner: old_owner,
                        token: old_token,
                        completion: PlaybackCompletion::Completed
                    },
                    "late"
                )?
                .events
                .is_empty());
        }
        assert_eq!(
            control
                .signal(
                    Signal::MediaPlaybackStarted {
                        owner: owner.clone(),
                        token
                    },
                    "now"
                )?
                .events
                .len(),
            1
        );
        let queued = control.apply(
            Action::PlayBridge {
                bridge: id("mix")?,
                id: id("queued")?,
                media: media("recording:saved")?,
            },
            "now",
            0,
        )?;
        assert!(queued.effects.is_empty());
        let deleted = control.apply(Action::DestroyBridge(id("mix")?), "now", 0)?;
        assert_eq!(
            deleted
                .events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::PlaybackFinished { .. }))
                .count(),
            2
        );
        assert!(matches!(
            control.apply(Action::GetPlayback(id("play")?), "now", 0),
            Err(ControlError::NotFound(_))
        ));
        previous = Some((owner, token));
    }
    Ok(())
}
fn admitted(control: &mut Control) -> Result<CallKey, ControlError> {
    control.subscribe("calls")?;
    control.apply(
        Action::Originate {
            id: id("phone")?,
            endpoint: "PJSIP/phone".into(),
            app: "calls".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "now",
        0,
    )?;
    control.key(&id("phone")?)
}

#[test]
fn playback_acceptance_start_and_completion_are_distinct_and_queued_audio_starts_in_order(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let key = admitted(&mut control)?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    let first = control.apply(
        Action::PlayChannel {
            channel: key.id.clone(),
            id: id("first")?,
            media: media("sound:welcome")?,
        },
        "now",
        1,
    )?;
    assert_eq!(first.status, 201);
    assert_eq!(
        first.body.as_ref().ok_or(ControlError::Capacity)?["state"],
        "queued"
    );
    assert!(first.events.is_empty());
    let [Effect::PlayMedia {
        owner: target,
        token: first_token,
        ..
    }] = &first.effects[..]
    else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(target, &rusterisk_server::MediaOwner::Channel(key.clone()));
    let first_token = *first_token;
    let second = control.apply(
        Action::PlayChannel {
            channel: key.id.clone(),
            id: id("second")?,
            media: media("recording:call-1")?,
        },
        "now",
        2,
    )?;
    assert!(second.effects.is_empty());
    let started = control.signal(
        Signal::PlaybackStarted {
            key: key.clone(),
            token: first_token,
        },
        "later",
    )?;
    assert!(
        matches!(&started.events[0].kind, EventKind::PlaybackStarted { playback } if playback.id == id("first")? && playback.state == rusterisk_api::PlaybackState::Playing)
    );
    assert!(control
        .signal(
            Signal::PlaybackStarted {
                key: key.clone(),
                token: first_token
            },
            "duplicate"
        )?
        .events
        .is_empty());
    let finished = control.signal(
        Signal::PlaybackFinished {
            key: key.clone(),
            token: first_token,
            completion: PlaybackCompletion::Completed,
        },
        "later",
    )?;
    assert!(
        matches!(&finished.events[0].kind, EventKind::PlaybackFinished { playback } if playback.id == id("first")? && playback.state == rusterisk_api::PlaybackState::Done)
    );
    let [Effect::PlayMedia {
        token: second_token,
        media: source,
        ..
    }] = &finished.effects[..]
    else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(source, &media("recording:call-1")?);
    let second_token = *second_token;
    assert!(matches!(
        control.apply(Action::GetPlayback(id("first")?), "later", 3),
        Err(ControlError::NotFound(_))
    ));
    control.signal(
        Signal::PlaybackStarted {
            key: key.clone(),
            token: second_token,
        },
        "later",
    )?;
    let stopped = control.apply(
        Action::ControlPlayback {
            id: id("second")?,
            operation: PlaybackControl::Stop,
        },
        "later",
        4,
    )?;
    assert!(
        matches!(&stopped.effects[..], [Effect::ControlPlayback { token, operation: PlaybackControl::Stop }] if *token == second_token)
    );
    assert!(stopped.events.is_empty());
    let finished = control.signal(
        Signal::PlaybackFinished {
            key,
            token: second_token,
            completion: PlaybackCompletion::Failed,
        },
        "later",
    )?;
    assert!(
        matches!(&finished.events[0].kind, EventKind::PlaybackFinished { playback } if playback.state == rusterisk_api::PlaybackState::Done)
    );
    Ok(())
}

#[test]
fn playback_requires_stasis_state_and_reserves_bounded_slots_without_replacing_existing_ids(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let key = admitted(&mut control)?;
    assert!(matches!(
        control.apply(
            Action::PlayChannel {
                channel: key.id.clone(),
                id: id("early")?,
                media: media("sound:welcome")?
            },
            "now",
            0
        ),
        Err(ControlError::Conflict(_))
    ));
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    for index in 0..8 {
        control.apply(
            Action::PlayChannel {
                channel: key.id.clone(),
                id: id(&format!("play-{index}"))?,
                media: media("sound:welcome")?,
            },
            "now",
            0,
        )?;
    }
    assert!(matches!(
        control.apply(
            Action::PlayChannel {
                channel: key.id.clone(),
                id: id("overflow")?,
                media: media("sound:welcome")?
            },
            "now",
            0
        ),
        Err(ControlError::Capacity)
    ));
    assert!(matches!(
        control.apply(
            Action::PlayChannel {
                channel: key.id.clone(),
                id: id("play-0")?,
                media: media("sound:other")?
            },
            "now",
            0
        ),
        Err(ControlError::Conflict(_))
    ));
    let existing = control
        .apply(Action::GetPlayback(id("play-0")?), "now", 0)?
        .body
        .ok_or(ControlError::Capacity)?;
    assert_eq!(existing["media_uri"], "sound:welcome");
    let ended = control.apply(Action::Hangup(key.id), "later", 1)?;
    assert_eq!(
        ended
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::PlaybackFinished { .. }))
            .count(),
        8
    );
    assert!(matches!(
        control.apply(Action::GetPlayback(id("play-0")?), "later", 1),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
