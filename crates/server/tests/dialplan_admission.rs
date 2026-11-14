use rusterisk_api::{ChannelId, ChannelState};
use rusterisk_server::{
    Action, Control, ControlError, IncomingCall, IncomingTarget, Milestone, Signal,
};

#[test]
fn dialplan_channels_exist_before_stasis_and_only_stasis_controls_require_an_app(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    let id = ChannelId::try_from("incoming".to_owned())?;
    let admitted = control.incoming(
        IncomingCall {
            id: id.clone(),
            endpoint: "phone".into(),
            caller: "100".into(),
            context: "internal".into(),
            extension: "200".into(),
            target: IncomingTarget::Dialplan,
        },
        "now",
    )?;
    assert!(admitted.events.is_empty());
    assert_eq!(control.channel(&id)?.state, ChannelState::Ring);
    assert_eq!(control.channel(&id)?.dialplan.app_name, "");
    assert_eq!(
        control
            .apply(Action::Answer(id.clone()), "now", 0)
            .map(|_| ()),
        Err(ControlError::Conflict("Channel not in Stasis application"))
    );
    assert_eq!(
        control
            .apply(
                Action::GetVariable {
                    channel: id.clone(),
                    variable: "EXTEN".into()
                },
                "now",
                0
            )
            .map(|_| ()),
        Err(ControlError::Conflict("Channel not in Stasis application"))
    );
    control.signal(
        Signal::Milestone {
            key: control.key(&id)?,
            milestone: Milestone::Answered,
        },
        "later",
    )?;
    assert_eq!(control.channel(&id)?.state, ChannelState::Up);
    let bridge = ChannelId::try_from("bridge".to_owned())?;
    control.apply(
        Action::CreateBridge {
            id: bridge.clone(),
            name: "Conversation".into(),
        },
        "now",
        0,
    )?;
    assert_eq!(
        control
            .apply(
                Action::AddChannels {
                    bridge,
                    channels: [id.clone()].into_iter().collect()
                },
                "now",
                0
            )
            .map(|_| ()),
        Err(ControlError::Conflict("Channel not in Stasis application"))
    );
    let ended = control.apply(Action::Hangup(id.clone()), "later", 0)?;
    assert!(ended.events.is_empty());
    assert!(matches!(
        control.channel(&id),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}
