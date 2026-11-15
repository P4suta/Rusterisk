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
            target: rusterisk_server::IncomingTarget::Stasis {
                application: "app".into(),
                arguments: Vec::new(),
            },
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

#[test]
fn reading_variables_before_stasis_preserves_write_authorization() -> Result<(), ControlError> {
    let mut control = Control::new(2);
    control.subscribe("app")?;
    control.apply(
        Action::Originate {
            id: id()?,
            endpoint: "PJSIP/phone".into(),
            app: "app".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "created",
        0,
    )?;
    assert_eq!(
        control.apply(get("CALLERID(num)")?, "read", 0)?.body,
        Some(serde_json::json!({"value": "100"}))
    );
    assert_eq!(
        control
            .apply(get("RUSTERISK_MEDIA_CONNECTED")?, "read", 0)?
            .body,
        Some(serde_json::json!({"value": "0"}))
    );
    assert!(matches!(
        control.apply(get("missing")?, "read", 0),
        Err(ControlError::NotFound("Provided variable was not found"))
    ));
    assert!(matches!(
        control.apply(set("X", Some("refused"))?, "write", 0),
        Err(ControlError::Conflict("Channel not in Stasis application"))
    ));
    Ok(())
}

#[test]
fn variable_reads_validate_missing_names_before_looking_up_the_channel() -> Result<(), ControlError>
{
    let mut control = Control::new(2);
    assert!(matches!(
        control.apply(get("")?, "read", 0),
        Err(ControlError::Invalid("Variable name is required"))
    ));
    assert!(matches!(
        control.apply(get("X")?, "read", 0),
        Err(ControlError::NotFound("Provided channel was not found"))
    ));
    Ok(())
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

#[test]
fn caller_functions_update_display_values_without_changing_endpoint_or_call_ownership(
) -> Result<(), ControlError> {
    let mut control = control()?;
    let key = control.key(&id()?)?;
    let name = control.channel(&id()?)?.name.clone();
    control.apply(set("CALLERID(name)", Some("Desk phone"))?, "now", 0)?;
    let updated = control.apply(set("CALLERID(num)", Some("300"))?, "now", 0)?;
    assert!(
        matches!(&updated.events[0].kind, EventKind::ChannelCallerId { caller_presentation: 0, channel, .. } if channel.caller.number == "300")
    );
    assert_eq!(
        control.apply(get("CALLERID(all)")?, "now", 0)?.body,
        Some(serde_json::json!({"value": "\"Desk phone\" <300>"}))
    );
    control.apply(set("CALLERID(all)", Some("\"Reception\" <400>"))?, "now", 0)?;
    assert_eq!(control.channel(&id()?)?.caller.name, "Reception");
    assert_eq!(control.channel(&id()?)?.caller.number, "400");
    assert!(control
        .apply(set("CALLERID(name)", Some(&"x".repeat(129)))?, "now", 0)
        .is_err());
    assert_eq!(control.channel(&id()?)?.caller.name, "Reception");
    assert_eq!(control.key(&id()?)?, key);
    assert_eq!(control.channel(&id()?)?.name, name);
    control.apply(set("CALLERID(num)", None)?, "now", 0)?;
    assert_eq!(control.channel(&id()?)?.caller.number, "");
    Ok(())
}
