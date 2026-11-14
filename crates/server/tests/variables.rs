use rusterisk_api::{ChannelId, EventKind};
use rusterisk_server::{Action, Control, ControlError, IncomingCall};

fn id() -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from("incoming".to_owned())?)
}

fn control() -> Result<Control, ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.incoming(
        IncomingCall {
            id: id()?,
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            app: "app".into(),
            args: Vec::new(),
        },
        "now",
    )?;
    Ok(control)
}

fn get(variable: &str) -> Result<Action, ControlError> {
    Ok(Action::GetVariable {
        channel: id()?,
        variable: variable.into(),
    })
}

fn set(variable: &str, value: Option<&str>) -> Result<Action, ControlError> {
    Ok(Action::SetVariable {
        channel: id()?,
        variable: variable.into(),
        value: value.map(String::from),
    })
}

#[test]
fn channel_variables_distinguish_missing_empty_and_unset_values() -> Result<(), ControlError> {
    let mut control = control()?;
    assert!(get("X")?.read_only());
    assert!(!set("X", Some(""))?.read_only());
    assert!(matches!(
        control.apply(get("X")?, "now", 0),
        Err(ControlError::NotFound(_))
    ));
    let changed = control.apply(set("X", Some("value"))?, "now", 0)?;
    assert!(
        matches!(&changed.events[0].kind, EventKind::ChannelVarset { variable, value, .. } if variable == "X" && value == "value")
    );
    assert_eq!(
        control.apply(get("X")?, "now", 0)?.body,
        Some(serde_json::json!({"value": "value"}))
    );
    control.apply(set("X", Some(""))?, "now", 0)?;
    assert_eq!(
        control.apply(get("X")?, "now", 0)?.body,
        Some(serde_json::json!({"value": ""}))
    );
    control.apply(set("X", None)?, "now", 0)?;
    assert!(matches!(
        control.apply(get("X")?, "now", 0),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}

#[test]
fn builtin_variables_follow_the_channel_snapshot_and_limits_preserve_existing_values(
) -> Result<(), ControlError> {
    let mut control = control()?;
    for (name, expected) in [
        ("EXTEN", "200"),
        ("CONTEXT", "internal"),
        ("PRIORITY", "1"),
        ("CALLERID(num)", "100"),
        ("CALLERID(name)", ""),
        ("RUSTERISK_MEDIA_CONNECTED", "0"),
    ] {
        assert_eq!(
            control.apply(get(name)?, "now", 0)?.body,
            Some(serde_json::json!({"value": expected}))
        );
    }
    for index in 0..64 {
        control.apply(set(&format!("X{index}"), Some("kept"))?, "now", 0)?;
    }
    assert!(matches!(
        control.apply(set("OVERFLOW", Some("refused"))?, "now", 0),
        Err(ControlError::Capacity)
    ));
    assert!(control
        .apply(set("X0", Some(&"x".repeat(4097)))?, "now", 0)
        .is_err());
    assert_eq!(
        control.apply(get("X0")?, "now", 0)?.body,
        Some(serde_json::json!({"value": "kept"}))
    );
    assert!(control
        .apply(set("EXTEN", Some("changed"))?, "now", 0)
        .is_err());
    assert!(control
        .apply(get("CHANNEL(unsupported)")?, "now", 0)
        .is_err());
    Ok(())
}
