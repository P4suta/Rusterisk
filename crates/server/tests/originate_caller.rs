use rusterisk_api::ChannelId;
use rusterisk_server::{Action, Control, ControlError, Effect};
use std::collections::BTreeMap;

fn action(caller: String) -> Result<Action, ControlError> {
    Ok(Action::Originate {
        id: ChannelId::try_from("caller-test".to_owned())?,
        endpoint: "PJSIP/phone".into(),
        app: "calls".into(),
        args: Vec::new(),
        caller,
        timeout: 30,
    })
}

#[test]
fn originate_parses_display_names_before_initial_variables_and_signaling(
) -> Result<(), ControlError> {
    let long_name = "a".repeat(128);
    for (source, expected_name, variables) in [
        ("101".to_owned(), "".to_owned(), BTreeMap::new()),
        (
            "Path caller <101>".to_owned(),
            "Path caller".to_owned(),
            BTreeMap::new(),
        ),
        (
            "\"Quoted caller\" <101>".to_owned(),
            "Quoted caller".to_owned(),
            BTreeMap::new(),
        ),
        (format!("{long_name} <101>"), long_name, BTreeMap::new()),
        (
            "Original <101>".to_owned(),
            "Replacement".to_owned(),
            BTreeMap::from([("CALLERID(name)".to_owned(), "Replacement".to_owned())]),
        ),
    ] {
        let mut control = Control::new(1);
        control.subscribe("calls")?;
        let result = control.apply_with_variables(action(source)?, variables, "created", 0)?;
        let channel = control.channel(&ChannelId::try_from("caller-test".to_owned())?)?;
        assert_eq!(channel.caller.name, expected_name);
        assert_eq!(channel.caller.number, "101");
        assert!(
            matches!(result.effects.as_slice(), [Effect::Start { endpoint, caller, .. }] if endpoint == "PJSIP/phone" && caller == "101")
        );
    }
    Ok(())
}

#[test]
fn invalid_originate_caller_identification_does_not_reserve_a_channel() -> Result<(), ControlError>
{
    let mut control = Control::new(1);
    control.subscribe("calls")?;
    for source in [
        "Caller <101".to_owned(),
        "Caller\r\nInjected <101>".to_owned(),
        format!("{} <101>", "a".repeat(129)),
    ] {
        assert!(matches!(
            control.apply(action(source)?, "rejected", 0),
            Err(ControlError::Invalid(_))
        ));
        assert!(control
            .channel(&ChannelId::try_from("caller-test".to_owned())?)
            .is_err());
    }
    control.apply(action("101".into())?, "created", 0)?;
    Ok(())
}
