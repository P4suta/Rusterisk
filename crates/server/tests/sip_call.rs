use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::{Codec, Config};
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::media::MediaHub;
use rusterisk_server::rtp::SocketPair;
use rusterisk_server::signaling::Signaling;
use rusterisk_server::{Action, ControlError, Controller, Credentials};
use rusterisk_sip::Session;
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

async fn answer(signal: &UdpSocket, audio: &SocketPair) -> Result<SocketAddr, ControlError> {
    let mut bytes = [0; 16385];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(2), signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let request = match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(ControlError::Invalid("Expected an invitation")),
    };
    let offered = Session::parse(
        std::str::from_utf8(&request.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?;
    let body = offered.answer(audio.local_addr().map_err(failure)?, 1);
    let reply = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=phone\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value(), signal.local_addr().map_err(failure)?, body.len());
    signal
        .send_to(reply.as_bytes(), peer)
        .await
        .map_err(failure)?;
    Ok(offered.remote)
}

async fn receive_audio(peer: &SocketPair, expected: i32) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = [0; 1501];
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
        Err(ControlError::Invalid("Expected received audio"))
    })
    .await
    .map_err(failure)?
}

#[tokio::test]
async fn control_requests_establish_two_sip_calls_and_bridge_received_audio(
) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let alice = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let bob = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let alice_audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let bob_audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[alice-contact]\ntype=aor\ncontact=sip:alice@{}\n[bob-contact]\ntype=aor\ncontact=sip:bob@{}\n[alice]\ntype=endpoint\ntransport=udp\nauth=auth\naors=alice-contact\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=auth\naors=bob-contact\nallow=ulaw\n", alice.local_addr().map_err(failure)?, bob.local_addr().map_err(failure)?);
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "",
    )
    .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let mut events = controller
            .subscribe(credentials()?, None, "app".into())
            .await?;
        for name in ["alice", "bob"] {
            controller
                .execute(
                    credentials()?,
                    None,
                    Action::Originate {
                        id: id(name)?,
                        endpoint: format!("PJSIP/{name}"),
                        app: "app".into(),
                        args: Vec::new(),
                        caller: "100".into(),
                        timeout: 30,
                    },
                )
                .await?;
        }
        let (alice_target, bob_target) =
            tokio::try_join!(answer(&alice, &alice_audio), answer(&bob, &bob_audio))?;
        let mut answered = BTreeSet::new();
        while answered.len() < 2 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.events.recv())
                .await
                .map_err(failure)?
                .ok_or(ControlError::Capacity)?;
            if let EventKind::ChannelStateChange { channel } = event.kind {
                if channel.state == ChannelState::Up {
                    answered.insert(channel.id);
                }
            }
        }
        controller
            .execute(
                credentials()?,
                None,
                Action::CreateBridge {
                    id: id("conversation")?,
                    name: "Conversation".into(),
                },
            )
            .await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::AddChannels {
                    bridge: id("conversation")?,
                    channels: [id("alice")?, id("bob")?].into_iter().collect(),
                },
            )
            .await?;
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?;
        for (phone, target, value) in [
            (&alice_audio, alice_target, 4000),
            (&bob_audio, bob_target, -7000),
        ] {
            let bytes = sender
                .packet(Frame::new(&[value; 160], 8000).map_err(failure)?)
                .map_err(failure)?
                .encode()
                .map_err(failure)?;
            phone
                .data()
                .send_to(&bytes, target)
                .await
                .map_err(failure)?;
        }
        tokio::try_join!(
            receive_audio(&alice_audio, -7000),
            receive_audio(&bob_audio, 4000)
        )?;
        controller
            .execute(credentials()?, None, Action::Hangup(id("alice")?))
            .await?;
        controller
            .execute(credentials()?, None, Action::Hangup(id("bob")?))
            .await?;
        Ok(())
    }
    .await;
    controller.shutdown();
    runner.await.map_err(failure)??;
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
