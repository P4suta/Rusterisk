use rusterisk_api::ChannelId;
use rusterisk_core::{DtmfDigit, DtmfTiming};
use rusterisk_server::{Action, CallKey, Control, ControlError, Effect, Milestone, Signal};

fn id() -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from("call".to_owned())?)
}
fn originate() -> Result<Action, ControlError> {
    Ok(Action::Originate {
        id: id()?,
        endpoint: "PJSIP/phone".into(),
        app: "app".into(),
        args: Vec::new(),
        caller: "100".into(),
        timeout: 30,
    })
}
fn send(digits: &str) -> Result<Action, ControlError> {
    Ok(Action::SendDtmf {
        channel: id()?,
        digits: digits.into(),
        timing: DtmfTiming::new(0, 100, 100, 0).map_err(|_| ControlError::Capacity)?,
    })
}
fn fill(control: &mut Control) -> Result<Vec<rusterisk_core::DtmfToken>, ControlError> {
    let mut tokens = Vec::new();
    for _ in 0..8 {
        let outcome = control.apply(send("#")?, "now", 0)?;
        assert!(outcome.body.is_none());
        assert!(outcome.events.is_empty());
        let [Effect::Dtmf { command, .. }] = outcome.effects.as_slice() else {
            return Err(ControlError::Capacity);
        };
        tokens.push(command.token);
    }
    assert_eq!(
        control.apply(send("#")?, "now", 0).err(),
        Some(ControlError::Capacity)
    );
    Ok(tokens)
}

#[test]
fn digit_admission_requires_answered_stasis_and_negotiated_support() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    control.apply(originate()?, "now", 0)?;
    assert_eq!(
        control
            .apply(send("#")?, "now", 0)
            .err()
            .map(|error| error.status()),
        Some(409)
    );
    let key = control.key(&id()?)?;
    control.signal(
        Signal::Milestone {
            key: key.clone(),
            milestone: Milestone::Answered,
        },
        "now",
    )?;
    assert_eq!(
        control.apply(send("#")?, "now", 0).err(),
        Some(ControlError::Unsupported)
    );
    control.signal(
        Signal::DtmfReady {
            key: key.clone(),
            events: 1 << DtmfDigit::Hash.event(),
        },
        "now",
    )?;
    assert_eq!(
        control.apply(send("1")?, "now", 0).err(),
        Some(ControlError::Unsupported)
    );
    for invalid in ["", "a", "X", "１", "012345678901234567890123456789012"] {
        assert_eq!(
            control
                .apply(send(invalid)?, "now", 0)
                .err()
                .map(|error| error.status()),
            Some(400)
        );
    }
    let tokens = fill(&mut control)?;
    control.signal(
        Signal::DtmfReady {
            key: key.clone(),
            events: u16::MAX,
        },
        "duplicate",
    )?;
    control.signal(
        Signal::DtmfFinished {
            key: CallKey {
                generation: key.generation + 1,
                ..key.clone()
            },
            token: tokens[0],
        },
        "wrong",
    )?;
    assert_eq!(
        control.apply(send("#")?, "now", 0).err(),
        Some(ControlError::Capacity)
    );
    control.signal(
        Signal::DtmfFinished {
            key: key.clone(),
            token: tokens[0],
        },
        "done",
    )?;
    control.apply(send("#")?, "now", 0)?;
    control.signal(
        Signal::DtmfFinished {
            key: key.clone(),
            token: tokens[0],
        },
        "duplicate",
    )?;
    assert_eq!(
        control.apply(send("#")?, "now", 0).err(),
        Some(ControlError::Capacity)
    );
    let ended = control.apply(Action::Hangup(id()?), "ended", 0)?;
    assert!(ended
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Release { key: released } if released == &key)));
    control.signal(Signal::MediaReleased { key: key.clone() }, "released")?;
    control.apply(originate()?, "replacement", 0)?;
    let replacement = control.key(&id()?)?;
    control.signal(
        Signal::DtmfReady {
            key: replacement.clone(),
            events: u16::MAX,
        },
        "ready",
    )?;
    control.signal(
        Signal::Milestone {
            key: replacement,
            milestone: Milestone::Answered,
        },
        "up",
    )?;
    fill(&mut control)?;
    control.signal(
        Signal::DtmfFinished {
            key,
            token: tokens[0],
        },
        "late",
    )?;
    assert_eq!(
        control.apply(send("#")?, "now", 0).err(),
        Some(ControlError::Capacity)
    );
    Ok(())
}
