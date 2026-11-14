use rusterisk_api::{ChannelId, EventKind};
use rusterisk_server::{Action, Control, ControlError, Milestone, Signal};

#[test]
fn repeated_media_notifications_emit_one_connection_event() -> Result<(), ControlError> {
    let mut control = Control::new(1);
    control.subscribe("app")?;
    let id = ChannelId::try_from("call".to_owned())?;
    control.apply(
        Action::Originate {
            id: id.clone(),
            endpoint: "PJSIP/phone".into(),
            app: "app".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "now",
        0,
    )?;
    let signal = Signal::Milestone {
        key: control.key(&id)?,
        milestone: Milestone::MediaConnected,
    };
    let first = control.signal(signal.clone(), "later")?;
    assert_eq!(first.events.len(), 1);
    assert!(matches!(
        &first.events[0].kind,
        EventKind::ChannelVarset { variable, value, .. }
            if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1"
    ));
    assert!(control.signal(signal, "again")?.events.is_empty());
    Ok(())
}
