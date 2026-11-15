use rusterisk_api::{ChannelId, EventKind};
use rusterisk_server::{Action, Control, ControlError};
use std::collections::BTreeMap;

fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}

fn originate(name: &str) -> Result<Action, ControlError> {
    Ok(Action::Originate {
        id: id(name)?,
        endpoint: "PJSIP/phone".into(),
        app: "app".into(),
        args: Vec::new(),
        caller: "100".into(),
        timeout: 30,
    })
}

#[test]
fn initial_values_precede_creation_events_and_remain_readable_before_stasis(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    let outcome = control.apply_with_variables(
        originate("call")?,
        BTreeMap::from([
            ("FIRST".into(), "one".into()),
            ("SECOND".into(), String::new()),
            ("CALLERID(name)".into(), "Alice".into()),
        ]),
        "created",
        0,
    )?;
    assert!(outcome.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::ChannelCreated { channel }
            if channel.caller.name == "Alice" && channel.caller.number == "100"
    )));
    for (variable, value) in [
        ("FIRST", "one"),
        ("SECOND", ""),
        ("CALLERID(name)", "Alice"),
    ] {
        assert_eq!(
            control
                .apply(
                    Action::GetVariable {
                        channel: id("call")?,
                        variable: variable.into()
                    },
                    "read",
                    0
                )?
                .body,
            Some(serde_json::json!({"value": value}))
        );
    }
    Ok(())
}

#[test]
fn invalid_initial_values_reject_the_entire_creation_before_reserving_its_identifier(
) -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    for invalid in [
        BTreeMap::from([
            ("FIRST".into(), "one".into()),
            ("LAST".into(), "bad\nvalue".into()),
        ]),
        BTreeMap::from([
            ("FIRST".into(), "one".into()),
            ("CALLERID(all)".into(), "bad <number".into()),
        ]),
        BTreeMap::from([
            ("FIRST".into(), "one".into()),
            ("EXTEN".into(), "changed".into()),
        ]),
        (0..65)
            .map(|index| (format!("V{index}"), "value".into()))
            .collect(),
    ] {
        assert!(control
            .apply_with_variables(originate("call")?, invalid, "created", 0)
            .is_err());
        assert!(control.channel(&id("call")?).is_err());
    }
    control.apply(originate("call")?, "created", 0)?;
    assert_eq!(control.channel(&id("call")?)?.caller.number, "100");
    assert!(control
        .apply_with_variables(
            Action::GetChannel(id("call")?),
            BTreeMap::from([("FIRST".into(), "one".into())]),
            "read",
            0
        )
        .is_err());
    assert!(control
        .apply(
            Action::GetVariable {
                channel: id("call")?,
                variable: "FIRST".into()
            },
            "read",
            0
        )
        .is_err());
    Ok(())
}
