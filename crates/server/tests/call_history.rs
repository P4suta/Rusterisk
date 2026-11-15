use rusterisk_api::ChannelId;
use rusterisk_config::Dialplan;
use rusterisk_server::{
    Action, CallHistoryEvent, Control, ControlError, IncomingCall, IncomingTarget, Signal,
};

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

#[test]
fn incoming_calls_have_history_before_stasis_and_terminal_events_are_emitted_once(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let channel = id("incoming")?;
    let admitted = control.incoming(
        IncomingCall {
            id: channel.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: IncomingTarget::Dialplan,
        },
        "started",
    )?;
    let key = control.key(&channel)?;
    assert_eq!(
        admitted.history,
        [CallHistoryEvent::Started {
            key: key.clone(),
            timestamp: "started".into()
        }]
    );
    let ended = control.start_dialplan(
        &key,
        Dialplan::parse("[internal]\nexten => 100,1,Hangup()\n")
            .map_err(|error| ControlError::Adapter(error.to_string()))?,
        0,
        "ended",
    )?;
    assert_eq!(
        ended.history,
        [CallHistoryEvent::Finished {
            key: key.clone(),
            timestamp: "ended".into()
        }]
    );
    assert!(control
        .signal(Signal::Terminated { key, cause: 16 }, "duplicate")?
        .history
        .is_empty());
    Ok(())
}

#[test]
fn outgoing_calls_keep_history_independent_of_an_app_entering_stasis() -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let channel = id("outgoing")?;
    let admitted = control.apply(
        Action::Originate {
            id: channel.clone(),
            endpoint: "PJSIP/phone".into(),
            app: "calls".into(),
            args: Vec::new(),
            caller: "100".into(),
            timeout: 30,
        },
        "started",
        0,
    )?;
    let key = control.key(&channel)?;
    assert_eq!(
        admitted.history,
        [CallHistoryEvent::Started {
            key: key.clone(),
            timestamp: "started".into()
        }]
    );
    let ended = control.apply(Action::Hangup(channel), "ended", 1)?;
    assert_eq!(
        ended.history,
        [CallHistoryEvent::Finished {
            key,
            timestamp: "ended".into()
        }]
    );
    assert!(control.shutdown("duplicate")?.history.is_empty());
    Ok(())
}
