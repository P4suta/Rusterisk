use super::{answer, exchange, failure, invite, response, AudioPeer};
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_config::{Codec, Config};
use rusterisk_media::{AudioFormat, RtpSender};
use rusterisk_server::{
    media::MediaHub, rtp::SocketPair, signaling::Signaling, ControlError, Controller,
};
use rusterisk_sip::Session;
use std::{
    fmt::Display,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{net::UdpSocket, sync::mpsc, task::JoinHandle};
#[path = "signals.rs"]
mod signals;
use signals::{acknowledge, TransferSignals};

async fn finish<T, E: Display>(
    mut task: JoinHandle<Result<T, E>>,
    limit: Duration,
) -> Result<T, ControlError> {
    match tokio::time::timeout(limit, &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => {
            Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
    }
}

fn pending_signal(socket: &UdpSocket) -> Result<String, ControlError> {
    let mut bytes = [0; 16385];
    match socket.try_recv_from(&mut bytes) {
        Ok((size, _)) => match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
            SipMessage::Response(response) => {
                Ok(format!("response {}", response.status_code.code()))
            }
            SipMessage::Request(request) => Ok(format!("request {}", request.method)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok("none".into()),
        Err(error) => Err(failure(error)),
    }
}

#[tokio::test]
async fn blind_transfer_notifies_after_source_bye_and_connects_received_audio(
) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let alice = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let bob = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let charlie = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let alice_audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let bob_audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let charlie_audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[alice-auth]\ntype=auth\nusername=phone\npassword=test-password\n[bob-auth]\ntype=auth\nusername=bob\npassword=bob-password\n[charlie-auth]\ntype=auth\nusername=charlie\npassword=charlie-password\n[spoofed-auth]\ntype=auth\nusername=spoofed\npassword=spoofed-password\n[alice-aor]\ntype=aor\n[bob-aor]\ntype=aor\ncontact=sip:bob@{}\n[charlie-aor]\ntype=aor\ncontact=sip:charlie@{}\n[spoofed-aor]\ntype=aor\n[spoofed]\ntype=endpoint\ntransport=udp\nauth=spoofed-auth\naors=spoofed-aor\ncontext=outside\nallow=ulaw\n[alice]\ntype=endpoint\ntransport=udp\nauth=alice-auth\naors=alice-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=bob-auth\naors=bob-aor\ncontext=internal\nallow=ulaw\n[charlie]\ntype=endpoint\ntransport=udp\nauth=charlie-auth\naors=charlie-aor\ncontext=internal\nallow=ulaw\n", bob.local_addr().map_err(failure)?, charlie.local_addr().map_err(failure)?);
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Dial(PJSIP/bob,5)\nsame => n,Hangup()\nexten => 200,1,Dial(PJSIP/charlie,5)\nsame => n,Hangup()\n[outside]\nexten => 100,1,Hangup()\n",
    ).map_err(failure)?;
    config.rtp.start = 0;
    config.rtp.end = 0;
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let addresses = [
        target,
        alice.local_addr().map_err(failure)?,
        bob.local_addr().map_err(failure)?,
        charlie.local_addr().map_err(failure)?,
        alice_audio.local_addr().map_err(failure)?,
        alice_audio.control_addr().map_err(failure)?,
        bob_audio.local_addr().map_err(failure)?,
        bob_audio.control_addr().map_err(failure)?,
        charlie_audio.local_addr().map_err(failure)?,
        charlie_audio.control_addr().map_err(failure)?,
    ];
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let (media_ports, mut server_ports) = mpsc::channel(3);
    let exchange = tokio::spawn(async move {
        invite(&alice, target, &alice_audio)
            .await
            .map_err(|error| failure(format!("authenticated invitation: {error}")))?;
        let bob_target =
            answer(&bob, &bob_audio)
                .await
                .map_err(|error| match pending_signal(&alice) {
                    Ok(observed) => failure(format!(
                        "outgoing peer answer: {error}; incoming {observed}"
                    )),
                    Err(observation) => failure(format!(
                        "outgoing peer answer: {error}; observation failed: {observation}"
                    )),
                })?;
        media_ports.try_send(bob_target).map_err(failure)?;
        let answered = response(&alice)
            .await
            .map_err(|error| failure(format!("incoming peer answer: {error}")))?;
        if answered.status_code.code() != 200 {
            return Err(ControlError::Invalid("Expected an answered call"));
        }
        let alice_target = Session::parse(
            std::str::from_utf8(&answered.body).map_err(failure)?,
            &[Codec::Pcmu],
        )
        .map_err(failure)?
        .remote;
        media_ports.try_send(alice_target).map_err(failure)?;
        let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-transfer-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
        alice
            .send_to(ack.as_bytes(), target)
            .await
            .map_err(failure)?;
        let mut left = AudioPeer {
            socket: &alice_audio,
            target: alice_target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?,
            sample: 4000,
        };
        let mut right = AudioPeer {
            socket: &bob_audio,
            target: bob_target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?,
            sample: -7000,
        };
        exchange(&mut left, &mut right, (-7000, 4000))
            .await
            .map_err(|error| failure(format!("initial received audio: {error}")))?;
        let refer = format!("REFER sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-transfer-refer;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContact: <sip:phone@{}>\r\nRefer-To: <sip:200@{target}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value(), alice.local_addr().map_err(failure)?);
        alice
            .send_to(refer.as_bytes(), target)
            .await
            .map_err(failure)?;
        let mut observed = TransferSignals::default();
        observed
            .accepted(&alice, target)
            .await
            .map_err(|error| failure(format!("REFER response: {error}")))?;
        observed
            .source_ended(&alice, target)
            .await
            .map_err(|error| failure(format!("source dialog departure: {error}")))?;
        let charlie_target = answer(&charlie, &charlie_audio)
            .await
            .map_err(|error| failure(format!("transfer target answer: {error}")))?;
        media_ports.try_send(charlie_target).map_err(failure)?;
        acknowledge(&charlie, target).await?;
        let mut third = AudioPeer {
            socket: &charlie_audio,
            target: charlie_target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [9; 10]).map_err(failure)?,
            sample: 9000,
        };
        let (progress, audio) = tokio::join!(
            observed.completed(&alice, target),
            exchange(&mut right, &mut third, (9000, -7000))
        );
        combine(
            progress.map_err(|error| failure(format!("final transfer notification: {error}"))),
            audio.map_err(|error| failure(format!("transferred received audio: {error}"))),
        )
    });
    let mut result = finish(exchange, Duration::from_secs(8)).await;
    controller.shutdown();
    let limit = Duration::from_secs(2);
    let (runner, signaling, hub) = tokio::join!(
        finish(runner, limit),
        finish(signaling, limit),
        finish(hub, limit)
    );
    result = combine(
        result,
        runner.map_err(|error| failure(format!("control shutdown: {error}"))),
    );
    result = combine(
        result,
        signaling.map_err(|error| failure(format!("signaling shutdown: {error}"))),
    );
    result = combine(
        result,
        hub.map_err(|error| failure(format!("media shutdown: {error}"))),
    );
    let mut addresses = Vec::from(addresses);
    for _ in 0..3 {
        match server_ports.try_recv() {
            Ok(address) => {
                addresses.push(address);
                match address.port().checked_add(1) {
                    Some(port) => addresses.push(SocketAddr::new(address.ip(), port)),
                    None => result = combine(result, Err(ControlError::Capacity)),
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(error) => {
                result = combine(result, Err(failure(error)));
                break;
            }
        }
    }
    let mut rebound = Vec::with_capacity(addresses.len());
    for address in addresses {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => {
                result = combine(result, Err(failure(format!("rebind {address}: {error}"))))
            }
        }
    }
    result
}
