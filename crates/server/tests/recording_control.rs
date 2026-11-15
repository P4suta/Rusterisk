use rusterisk_api::{ChannelId, EventKind, RecordingState};
use rusterisk_core::RecordingControl;
use rusterisk_server::media::RecordingEnd;
use rusterisk_server::{Action, Control, ControlError, Effect, MediaOwner, Signal};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

#[test]
fn completion_after_app_disconnection_cannot_route_to_a_replacement_subscription(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.incoming(
        rusterisk_server::IncomingCall {
            id: id("phone")?,
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: rusterisk_server::IncomingTarget::Stasis {
                application: "calls".into(),
                arguments: Vec::new(),
            },
        },
        "now",
    )?;
    let accepted = control.apply(
        Action::RecordChannel {
            channel: id("phone")?,
            name: id("saved")?,
        },
        "now",
        0,
    )?;
    let [Effect::RecordMedia { owner, token, .. }] = &accepted.effects[..] else {
        return Err(ControlError::Unsupported);
    };
    control.signal(
        Signal::RecordingAdmitted {
            owner: owner.clone(),
            token: *token,
            result: Ok(()),
        },
        "now",
    )?;
    control.unsubscribe("calls", "now")?;
    control.subscribe("calls")?;
    let finished = control.signal(
        Signal::RecordingFinished {
            owner: owner.clone(),
            token: *token,
            result: Ok(()),
        },
        "later",
    )?;
    assert!(finished.events.is_empty());
    Ok(())
}

#[test]
fn recording_admission_controls_and_publication_are_separate_owned_transitions(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.apply(
        Action::CreateBridge {
            id: id("mix")?,
            name: String::new(),
        },
        "now",
        0,
    )?;
    let accepted = control.apply(
        Action::RecordBridge {
            bridge: id("mix")?,
            name: id("saved")?,
        },
        "now",
        0,
    )?;
    assert_eq!(accepted.status, 201);
    assert_eq!(
        accepted.location.as_deref(),
        Some("/ari/recordings/live/saved")
    );
    assert_eq!(
        accepted.body.ok_or(ControlError::Capacity)?["state"],
        "queued"
    );
    assert!(accepted.events.is_empty());
    let [Effect::RecordMedia { owner, token, name }] = &accepted.effects[..] else {
        return Err(ControlError::Unsupported);
    };
    assert_eq!(name, &id("saved")?);
    let owner = owner.clone();
    let token = *token;
    assert!(matches!(owner, MediaOwner::Bridge { .. }));
    assert!(matches!(
        control.apply(
            Action::ControlRecording {
                name: id("saved")?,
                operation: RecordingControl::Pause
            },
            "now",
            0
        ),
        Err(ControlError::Conflict(_))
    ));
    assert!(control
        .apply(
            Action::RecordBridge {
                bridge: id("mix")?,
                name: id("other")?
            },
            "now",
            0
        )
        .is_err());
    let admitted = control.signal(
        Signal::RecordingAdmitted {
            owner: owner.clone(),
            token,
            result: Ok(()),
        },
        "now",
    )?;
    assert!(
        matches!(&admitted.events[0].kind, EventKind::RecordingStarted { recording } if recording.state == RecordingState::Recording)
    );
    assert!(control
        .signal(
            Signal::RecordingAdmitted {
                owner: owner.clone(),
                token,
                result: Ok(())
            },
            "duplicate"
        )?
        .events
        .is_empty());
    for operation in [
        RecordingControl::Mute,
        RecordingControl::Pause,
        RecordingControl::Resume,
        RecordingControl::Unmute,
    ] {
        let changed = control.apply(
            Action::ControlRecording {
                name: id("saved")?,
                operation,
            },
            "now",
            0,
        )?;
        assert!(
            matches!(&changed.effects[..], [Effect::ControlRecording { token: current, operation: applied }] if *current == token && *applied == operation)
        );
    }
    let stopped = control.apply(
        Action::EndRecording {
            name: id("saved")?,
            mode: RecordingEnd::Finish,
        },
        "now",
        0,
    )?;
    assert!(
        matches!(&stopped.effects[..], [Effect::EndRecording { token: current, mode: RecordingEnd::Finish }] if *current == token)
    );
    assert!(stopped.events.is_empty());
    let finished = control.signal(
        Signal::RecordingFinished {
            owner,
            token,
            result: Ok(()),
        },
        "now",
    )?;
    assert!(
        matches!(&finished.events[0].kind, EventKind::RecordingFinished { recording } if recording.state == RecordingState::Done)
    );
    assert!(matches!(
        control.apply(Action::GetRecording(id("saved")?), "now", 0),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}

#[test]
fn failed_admission_releases_ownership_and_deletion_waits_for_file_completion(
) -> Result<(), ControlError> {
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
        let accepted = control.apply(
            Action::RecordBridge {
                bridge: id("mix")?,
                name: id("failed")?,
            },
            "now",
            0,
        )?;
        let [Effect::RecordMedia { owner, token, .. }] = &accepted.effects[..] else {
            return Err(ControlError::Unsupported);
        };
        let failed = control.signal(
            Signal::RecordingAdmitted {
                owner: owner.clone(),
                token: *token,
                result: Err(ControlError::Conflict("Recording already exists")),
            },
            "now",
        )?;
        assert!(
            matches!(&failed.events[0].kind, EventKind::RecordingFailed { recording } if recording.state == RecordingState::Failed)
        );
        let accepted = control.apply(
            Action::RecordBridge {
                bridge: id("mix")?,
                name: id("current")?,
            },
            "now",
            0,
        )?;
        let [Effect::RecordMedia { owner, token, .. }] = &accepted.effects[..] else {
            return Err(ControlError::Unsupported);
        };
        if let Some((old_owner, old_token)) = previous {
            assert_ne!(old_owner, *owner);
            assert!(control
                .signal(
                    Signal::RecordingFinished {
                        owner: old_owner,
                        token: old_token,
                        result: Ok(())
                    },
                    "late"
                )?
                .events
                .is_empty());
        }
        control.signal(
            Signal::RecordingAdmitted {
                owner: owner.clone(),
                token: *token,
                result: Ok(()),
            },
            "now",
        )?;
        let deleted = control.apply(Action::DestroyBridge(id("mix")?), "now", 0)?;
        assert!(!deleted
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::RecordingFinished { .. })));
        let finished = control.signal(
            Signal::RecordingFinished {
                owner: owner.clone(),
                token: *token,
                result: Ok(()),
            },
            "now",
        )?;
        assert!(matches!(
            &finished.events[0].kind,
            EventKind::RecordingFinished { .. }
        ));
        previous = Some((owner.clone(), *token));
    }
    Ok(())
}
