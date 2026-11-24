use rusterisk_api::{ChannelId, EventKind, ExternalRtpTarget, StaticRtpCodec, Variable};
use rusterisk_config::Config;
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::{
    media::MediaHub, rtp::SocketPair, signaling::Signaling, Action, ControlError, Controller,
    Credentials, Subscription,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

async fn started(
    subscription: &mut Subscription,
    expected: &ChannelId,
) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..16 {
            let event = subscription
                .events
                .recv()
                .await
                .ok_or(ControlError::Capacity)?;
            match event.kind {
                EventKind::StasisStart { channel, .. } if channel.id == *expected => return Ok(()),
                EventKind::ChannelDestroyed { channel, .. } if channel.id == *expected => {
                    return Err(ControlError::Adapter(
                        "External media ended before Stasis".into(),
                    ))
                }
                EventKind::StasisStart { .. }
                | EventKind::StasisEnd { .. }
                | EventKind::ChannelCreated { .. }
                | EventKind::ChannelStateChange { .. }
                | EventKind::ChannelDialplan { .. }
                | EventKind::ChannelDestroyed { .. }
                | EventKind::ChannelVarset { .. } => {}
                EventKind::ChannelCallerId { .. }
                | EventKind::Published(_)
                | EventKind::ChannelHangupRequest { .. }
                | EventKind::ChannelEnteredBridge { .. }
                | EventKind::ChannelLeftBridge { .. }
                | EventKind::BridgeCreated { .. }
                | EventKind::BridgeDestroyed { .. }
                | EventKind::ChannelDtmfReceived { .. }
                | EventKind::ChannelHold { .. }
                | EventKind::ChannelUnhold { .. }
                | EventKind::PlaybackStarted { .. }
                | EventKind::PlaybackContinuing { .. }
                | EventKind::PlaybackFinished { .. }
                | EventKind::RecordingStarted { .. }
                | EventKind::RecordingFinished { .. }
                | EventKind::RecordingFailed { .. }
                | EventKind::ApplicationReplaced { .. }
                | EventKind::ApplicationMoveFailed { .. }
                | EventKind::Dial { .. }
                | EventKind::ChannelUserevent { .. }
                | EventKind::EndpointStateChange { .. } => return Err(ControlError::Unsupported),
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(|error| failure(format!("External Stasis deadline: {error}")))?
}

async fn local(controller: &Controller, channel: &ChannelId) -> Result<SocketAddr, ControlError> {
    let mut values = Vec::new();
    for variable in ["UNICASTRTP_LOCAL_ADDRESS", "UNICASTRTP_LOCAL_PORT"] {
        let response = controller
            .execute(
                credentials()?,
                None,
                Action::GetVariable {
                    channel: channel.clone(),
                    variable: variable.into(),
                },
            )
            .await?;
        let value: Variable = serde_json::from_value(response.body.ok_or(ControlError::Capacity)?)?;
        values.push(value.value);
    }
    format!("{}:{}", values[0], values[1])
        .parse()
        .map_err(failure)
}

async fn receives(peer: &SocketPair, expected: i32) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = [0; 2049];
        for _ in 0..100 {
            let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
            let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
            if packet.payload().len() == 160
                && packet
                    .payload()
                    .iter()
                    .all(|byte| (i32::from(mulaw_decode(*byte)) - expected).abs() < 300)
            {
                return Ok(());
            }
        }
        Err(ControlError::Invalid("Expected external audio"))
    })
    .await
    .map_err(|error| failure(format!("External audio deadline for {expected}: {error}")))?
}

#[tokio::test]
async fn external_channels_bridge_received_audio_and_release_both_socket_pairs(
) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let left = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let right = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result: Result<[SocketAddr; 2], ControlError> = async {
        let mut subscription = controller
            .subscribe(credentials()?, None, "calls".into())
            .await?;
        for (name, peer) in [("left", &left), ("right", &right)] {
            controller
                .execute(
                    credentials()?,
                    None,
                    Action::ExternalRtp {
                        id: id(name)?,
                        app: "calls".into(),
                        target: ExternalRtpTarget::new(
                            peer.local_addr().map_err(failure)?,
                            StaticRtpCodec::Pcmu,
                        )
                        .map_err(failure)?,
                        data: String::new(),
                    },
                )
                .await?;
            started(&mut subscription, &id(name)?).await?;
        }
        let first = local(&controller, &id("left")?).await?;
        let second = local(&controller, &id("right")?).await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::CreateBridge {
                    id: id("mix")?,
                    name: "External audio".into(),
                },
            )
            .await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::AddChannels {
                    bridge: id("mix")?,
                    channels: [id("left")?, id("right")?].into_iter().collect(),
                },
            )
            .await?;
        let send = async {
            let mut first_sender =
                RtpSender::new(AudioFormat::Pcmu, 0, [19; 10]).map_err(failure)?;
            let mut second_sender =
                RtpSender::new(AudioFormat::Pcmu, 0, [20; 10]).map_err(failure)?;
            let mut clock = tokio::time::interval(Duration::from_millis(20));
            clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for _ in 0..15 {
                clock.tick().await;
                for (peer, destination, sender, value) in [
                    (&left, first, &mut first_sender, 4000),
                    (&right, second, &mut second_sender, -7000),
                ] {
                    let packet = sender
                        .packet(Frame::new(&[value; 160], 8000).map_err(failure)?)
                        .map_err(failure)?
                        .encode()
                        .map_err(failure)?;
                    peer.data()
                        .send_to(&packet, destination)
                        .await
                        .map_err(failure)?;
                }
            }
            Ok::<(), ControlError>(())
        };
        tokio::try_join!(send, receives(&left, -7000), receives(&right, 4000))?;
        for name in ["left", "right"] {
            controller
                .execute(credentials()?, None, Action::Hangup(id(name)?))
                .await?;
        }
        Ok([first, second])
    }
    .await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    for address in result? {
        let replacement =
            SocketPair::bind(address.ip(), address.port(), address.port() + 1).map_err(failure)?;
        assert_eq!(replacement.local_addr().map_err(failure)?, address);
    }
    Ok(())
}
