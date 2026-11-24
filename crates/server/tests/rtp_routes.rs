use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::{Codec, Config};
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::{
    media::MediaHub, rtp::SocketPair, signaling::Signaling, Action, ControlError, Controller,
    Credentials,
};
use rusterisk_sip::Session;
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::net::UdpSocket;

#[path = "rtp_routes/incoming.rs"]
mod incoming;

enum CallOrigin {
    Device,
    Controller,
}

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn destination(body: &[u8]) -> Result<SocketAddr, ControlError> {
    let session = Session::parse(std::str::from_utf8(body).map_err(failure)?, &[Codec::Pcmu])
        .map_err(failure)?;
    if session.remote.ip().is_unspecified() || session.remote.port() == 0 {
        return Err(ControlError::Invalid(
            "An active RTP session advertised an unspecified destination",
        ));
    }
    Ok(session.remote)
}

async fn answer(signal: &UdpSocket, audio: &SocketPair) -> Result<SocketAddr, ControlError> {
    let mut bytes = [0; 16385];
    let (size, server) = signal.recv_from(&mut bytes).await.map_err(failure)?;
    let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected an invitation"));
    };
    assert_eq!(request.method, Method::Invite);
    let target = destination(&request.body)?;
    let offered = Session::parse(
        std::str::from_utf8(&request.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?;
    let body = offered
        .answer(audio.local_addr().map_err(failure)?, 1)
        .map_err(failure)?;
    let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=phone\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value(), signal.local_addr().map_err(failure)?, body.len());
    signal
        .send_to(response.as_bytes(), server)
        .await
        .map_err(failure)?;
    Ok(target)
}

async fn received(peer: &SocketPair, expected: i32) -> Result<(), ControlError> {
    let mut bytes = [0; 1501];
    let mut consecutive = 0;
    for _ in 0..100 {
        let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
        let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
        if packet.payload_type() == 0
            && packet.payload().len() == 160
            && packet
                .payload()
                .iter()
                .all(|sample| (i32::from(mulaw_decode(*sample)) - expected).abs() < 300)
        {
            consecutive += 1;
            if consecutive == 6 {
                return Ok(());
            }
        } else {
            consecutive = 0;
        }
    }
    Err(ControlError::Invalid("Expected received bridge audio"))
}

async fn exchange(peers: &[SocketPair; 2], targets: &[SocketAddr]) -> Result<(), ControlError> {
    let mut senders = [
        RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?,
        RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?,
    ];
    let transmit = async {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..100 {
            clock.tick().await;
            for (index, sample) in [4000, -7000].into_iter().enumerate() {
                let packet = senders[index]
                    .packet(Frame::new(&[sample; 160], 8000).map_err(failure)?)
                    .map_err(failure)?
                    .encode()
                    .map_err(failure)?;
                peers[index]
                    .data()
                    .send_to(&packet, targets[index])
                    .await
                    .map_err(failure)?;
            }
        }
        Err::<(), ControlError>(ControlError::Invalid("Audio exceeded its packet bound"))
    };
    let receive = async {
        tokio::try_join!(received(&peers[0], -7000), received(&peers[1], 4000))?;
        Ok::<(), ControlError>(())
    };
    tokio::select! {
        result = transmit => result,
        result = receive => result,
    }
}

async fn bridge_calls(origin: CallOrigin) -> Result<(), ControlError> {
    let signals = [
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
    ];
    let audio = [
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
    ];
    let mut sip = "[udp]\ntype=transport\nprotocol=udp\nbind=0.0.0.0:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n".to_owned();
    for (name, signal) in ["left", "right"].into_iter().zip(&signals) {
        sip.push_str(&format!("[{name}-aor]\ntype=aor\ncontact=sip:phone@{}\n[{name}]\ntype=endpoint\ntransport=udp\nauth=auth\naors={name}-aor\nallow=ulaw\ndtmf_mode=info\ncontext=internal\n", signal.local_addr().map_err(failure)?));
    }
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    )
    .map_err(failure)?;
    config.rtp.start = 0;
    config.rtp.end = 0;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let mut target = signaling
        .listeners()?
        .first()
        .ok_or(ControlError::Invalid("Missing SIP listener"))?
        .1;
    target.set_ip(Ipv4Addr::LOCALHOST.into());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = tokio::time::timeout(Duration::from_secs(6), async {
        let credentials = Credentials::new("app".into(), "test-password".into())?;
        let mut events = controller
            .subscribe(credentials.clone(), None, "calls".into())
            .await?;
        let mut ids = Vec::with_capacity(2);
        let mut targets = Vec::with_capacity(2);
        for (index, name) in ["left", "right"].into_iter().enumerate() {
            if index == 0 && matches!(origin, CallOrigin::Device) {
                let (id, target) = incoming::answer_device(
                    &signals[index],
                    &audio[index],
                    target,
                    &controller,
                    &credentials,
                    &mut events.events,
                )
                .await?;
                ids.push(id);
                targets.push(target);
                continue;
            }
            let id = ChannelId::try_from(name.to_owned())?;
            controller
                .execute(
                    credentials.clone(),
                    None,
                    Action::Originate {
                        id: id.clone(),
                        endpoint: format!("PJSIP/{name}"),
                        app: "calls".into(),
                        args: Vec::new(),
                        caller: "100".into(),
                        timeout: 3,
                    },
                )
                .await?;
            targets.push(answer(&signals[index], &audio[index]).await?);
            loop {
                if let EventKind::StasisStart { channel, .. } = events
                    .events
                    .recv()
                    .await
                    .ok_or(ControlError::Capacity)?
                    .kind
                {
                    if channel.id == id {
                        assert_eq!(channel.state, ChannelState::Up);
                        break;
                    }
                }
            }
            ids.push(id);
        }
        let bridge = ChannelId::try_from("rtp-route-bridge".to_owned())?;
        controller
            .execute(
                credentials.clone(),
                None,
                Action::CreateBridge {
                    id: bridge.clone(),
                    name: "RTP routes".into(),
                },
            )
            .await?;
        controller
            .execute(
                credentials.clone(),
                None,
                Action::AddChannels {
                    bridge: bridge.clone(),
                    channels: ids.clone(),
                },
            )
            .await?;
        exchange(&audio, &targets).await?;
        for id in ids {
            controller
                .execute(credentials.clone(), None, Action::Hangup(id))
                .await?;
        }
        controller
            .execute(credentials, None, Action::DestroyBridge(bridge))
            .await?;
        Ok::<(), ControlError>(())
    })
    .await
    .map_err(failure);
    controller.shutdown();
    let signaling = signaling.await.map_err(failure);
    let hub = hub.await.map_err(failure);
    result??;
    signaling??;
    hub?.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn wildcard_outgoing_rtp_offers_advertise_received_audio_destinations(
) -> Result<(), ControlError> {
    bridge_calls(CallOrigin::Controller).await
}

#[tokio::test]
async fn wildcard_incoming_rtp_answers_advertise_received_audio_destinations(
) -> Result<(), ControlError> {
    bridge_calls(CallOrigin::Device).await
}
