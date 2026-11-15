use rusterisk_api::{
    Channel, ChannelId, ChannelState, MediaControlFormat, MediaDirection, MediaFormat,
};
use rusterisk_server::{Action, Control, ControlError, Milestone, Signal};
use rusterisk_server::{Controller, Credentials};
use std::time::Duration;

fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

fn create(connection: &str) -> Result<Action, ControlError> {
    create_format(connection, MediaFormat::Pcmu)
}

fn create_format(connection: &str, format: MediaFormat) -> Result<Action, ControlError> {
    Ok(Action::ExternalWebSocket {
        id: id("external")?,
        app: "calls".into(),
        connection_id: id(connection)?,
        format,
        control_format: Some(MediaControlFormat::Json),
        direction: MediaDirection::Both,
        auto_answer: true,
        data: "session".into(),
    })
}

#[test]
fn incoming_opus_admission_publishes_packet_mode_and_preserves_the_format(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let outcome = control.apply(create_format("packet", MediaFormat::Opus)?, "created", 0)?;
    let channel: Channel = serde_json::from_value(outcome.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(
        channel
            .channelvars
            .ok_or(ControlError::Capacity)?
            .get("MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE")
            .map(String::as_str),
        Some("0")
    );
    let (key, options) = control.claim_media_connection(&id("packet")?)?;
    assert_eq!(options.format, MediaFormat::Opus);
    control.apply(Action::Hangup(key.id), "ended", 1)?;
    assert!(control.claim_media_connection(&id("packet")?).is_err());
    Ok(())
}

#[test]
fn incoming_media_connections_publish_variables_and_claim_one_channel_generation(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    let accepted = control.apply_with_variables(
        create("connection")?,
        std::collections::BTreeMap::from([("INITIAL_TAG".into(), "incoming value".into())]),
        "created",
        0,
    )?;
    let channel: Channel = serde_json::from_value(accepted.body.ok_or(ControlError::Capacity)?)?;
    assert_eq!(channel.state, ChannelState::Down);
    let variables = channel.channelvars.ok_or(ControlError::Capacity)?;
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_CONNECTION_ID")
            .map(String::as_str),
        Some("connection")
    );
    assert_eq!(
        variables
            .get("MEDIA_WEBSOCKET_OPTIMAL_FRAME_SIZE")
            .map(String::as_str),
        Some("160")
    );
    let original = control.key(&channel.id)?;
    control.signal(
        Signal::Milestone {
            key: original.clone(),
            milestone: Milestone::Answered,
        },
        "early",
    )?;
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Down);
    let (claimed, options) = control.claim_media_connection(&id("connection")?)?;
    assert_eq!(claimed, original);
    assert_eq!(options.format, MediaFormat::Pcmu);
    assert_eq!(
        options
            .channel_variables
            .get("INITIAL_TAG")
            .map(String::as_str),
        Some("incoming value")
    );
    assert_eq!(
        options
            .channel_variables
            .get("MEDIA_WEBSOCKET_CONNECTION_ID")
            .map(String::as_str),
        Some("connection")
    );
    assert!(matches!(
        control.claim_media_connection(&id("connection")?),
        Err(ControlError::Conflict(
            "Another websocket connection exists for this connection id"
        ))
    ));
    control.signal(
        Signal::Milestone {
            key: original.clone(),
            milestone: Milestone::Answered,
        },
        "answered",
    )?;
    assert_eq!(control.channel(&channel.id)?.state, ChannelState::Up);
    control.apply(Action::Hangup(channel.id.clone()), "ended", 1)?;
    assert!(matches!(
        control.claim_media_connection(&id("connection")?),
        Err(ControlError::NotFound("WebSocket instance not found"))
    ));
    control.apply(create("replacement")?, "replacement", 2)?;
    let replacement = control.key(&channel.id)?;
    control.signal(
        Signal::Terminated {
            key: original,
            cause: 16,
        },
        "late",
    )?;
    assert_eq!(control.key(&channel.id)?, replacement);
    assert_eq!(
        control.claim_media_connection(&id("replacement")?)?.0,
        replacement
    );
    Ok(())
}

#[test]
fn unconnected_media_channels_expire_at_the_reference_originate_deadline(
) -> Result<(), ControlError> {
    let mut control = Control::new(4);
    control.subscribe("calls")?;
    control.apply(create("waiting")?, "created", 100)?;
    control.expire(30099, "before")?;
    assert!(control.channel(&id("external")?).is_ok());
    control.expire(30100, "expired")?;
    assert!(matches!(
        control.claim_media_connection(&id("waiting")?),
        Err(ControlError::NotFound(_))
    ));
    Ok(())
}

#[tokio::test]
async fn dropped_connection_leases_release_only_their_channel_generation(
) -> Result<(), ControlError> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(|error| ControlError::Adapter(error.to_string()))?;
    let (controller, runner, _effects) = Controller::new(config, 4);
    let task = tokio::spawn(runner.run());
    let credentials = Credentials::new("app".into(), "test-password".into())?;
    let result = async {
        let _subscription = controller
            .subscribe(credentials.clone(), None, "calls".into())
            .await?;
        controller
            .execute(credentials.clone(), None, create("original")?)
            .await?;
        assert!(matches!(
            controller
                .claim_media_connection(id("original")?, Some("https://refused.example".into()))
                .await,
            Err(ControlError::Forbidden)
        ));
        let original = controller
            .claim_media_connection(id("original")?, None)
            .await?;
        assert_eq!(original.options.channel_id, id("external")?);
        assert!(matches!(
            controller
                .claim_media_connection(id("original")?, None)
                .await,
            Err(ControlError::Conflict(_))
        ));
        controller
            .execute(credentials.clone(), None, Action::Hangup(id("external")?))
            .await?;
        controller
            .execute(credentials.clone(), None, create("replacement")?)
            .await?;
        let replacement = controller
            .claim_media_connection(id("replacement")?, None)
            .await?;
        assert_ne!(original.key, replacement.key);
        drop(original);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::GetChannel(id("external")?)
                )
                .await?
                .status,
            200
        );
        drop(replacement);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match controller
                    .execute(
                        credentials.clone(),
                        None,
                        Action::GetChannel(id("external")?),
                    )
                    .await
                {
                    Err(ControlError::NotFound(_)) => return Ok::<(), ControlError>(()),
                    Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(error) => return Err(error),
                }
            }
        })
        .await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
        Ok(())
    }
    .await;
    controller.shutdown();
    task.await
        .map_err(|error| ControlError::Adapter(error.to_string()))??;
    result
}
