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
    let [Effect::PlayChannel {
        key: target,
        token: first_token,
        ..
    }] = &first.effects[..]
    else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(target, &key);
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
    let [Effect::PlayChannel {
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
