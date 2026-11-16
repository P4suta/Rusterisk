use super::{
    CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use rusterisk_api::ChannelId;

fn incoming(control: &mut Control, id: &str, endpoint: &str) -> Result<CallKey, ControlError> {
    let id = ChannelId::try_from(id.to_owned())?;
    control.incoming(
        IncomingCall {
            id: id.clone(),
            endpoint: endpoint.into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "100".into(),
            target: IncomingTarget::Dialplan,
        },
        "incoming",
    )?;
    control.key(&id)
}

#[test]
fn dial_transfer_requires_exactly_the_two_live_bridge_generations() -> Result<(), ControlError> {
    let program =
        rusterisk_config::Dialplan::parse("[internal]\nexten => 100,1,Dial(PJSIP/bob,5)\n")
            .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let mut control = Control::with_dialplan(8, program.clone());
    let parent = incoming(&mut control, "parent", "alice")?;
    let started = control.start_dialplan(&parent, program, 0, "start")?;
    let [Effect::Start { key: child, .. }] = started.effects.as_slice() else {
        return Err(ControlError::Invalid("Expected one Dial child"));
    };
    let child = child.clone();
    control.signal_at(
        Signal::Milestone {
            key: child.clone(),
            milestone: Milestone::Answered,
        },
        "child answer",
        1,
    )?;
    let joined = control.signal_at(
        Signal::Milestone {
            key: parent.clone(),
            milestone: Milestone::Answered,
        },
        "parent answer",
        2,
    )?;
    let [Effect::DialBridge {
        generation: bridge, ..
    }] = joined.effects.as_slice()
    else {
        return Err(ControlError::Invalid("Expected Dial bridge"));
    };
    let bridge = *bridge;
    assert!(control.transfer_dial_pair(&parent).is_ok());
    assert!(control.transfer_dial_pair(&child).is_ok());
    let third = incoming(&mut control, "third", "charlie")?;
    control.signal_at(
        Signal::Milestone {
            key: third.clone(),
            milestone: Milestone::Answered,
        },
        "third answer",
        3,
    )?;
    let entry = control
        .channels
        .get_mut(&third.id)
        .ok_or(ControlError::NotFound("Missing third channel"))?;
    entry.call = entry.call.join(bridge)?;
    let retained: Vec<_> = control
        .channels
        .iter()
        .map(|(id, entry)| (id.clone(), entry.call))
        .collect();
    assert!(matches!(
        control.transfer_dial_pair(&parent),
        Err(ControlError::Unsupported)
    ));
    assert!(matches!(
        control.transfer_dial_pair(&child),
        Err(ControlError::Unsupported)
    ));
    let current: Vec<_> = control
        .channels
        .iter()
        .map(|(id, entry)| (id.clone(), entry.call))
        .collect();
    assert_eq!(current, retained);
    Ok(())
}
