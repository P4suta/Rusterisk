use super::{
    failure,
    transfer_lifetime::{combine, finish},
    transfer_peer::Peer,
    transfer_signals as signals,
};
use rusterisk_config::Config;
use rusterisk_server::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{net::UdpSocket, sync::mpsc};
#[path = "exercise.rs"]
mod exercise;
#[path = "reference.rs"]
mod reference;
use exercise::{Departure, ObservedTransfer};
use signals::RequestRoute;

#[tokio::test]
async fn blind_transfer_notifies_after_source_bye_and_connects_received_audio(
) -> Result<(), ControlError> {
    native_transfer(Departure::Server).await.map(|_| ())
}

#[tokio::test]
async fn transferor_bye_after_refer_acceptance_keeps_target_audio_and_notifications(
) -> Result<(), ControlError> {
    native_transfer(Departure::Transferor).await.map(|_| ())
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn reference_blind_transfer_matches_native_received_audio() -> Result<(), ControlError> {
    let native = native_transfer(Departure::Transferor).await?;
    let reference = reference::compare().await?;
    println!("Observed transfer: native event={:?}, initial_before_answer={}, server_bye_before_answer={}; Asterisk event={:?}, initial_before_answer={}, server_bye_before_answer={}", native.event, native.initial_before_answer, native.server_bye_before_answer, reference.event, reference.initial_before_answer, reference.server_bye_before_answer);
    Ok(())
}

async fn native_transfer(departure: Departure) -> Result<ObservedTransfer, ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let alice = Peer::bind(ip, ip).await?;
    let bob = Peer::bind(ip, ip).await?;
    let charlie = Peer::bind(ip, ip).await?;
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[alice-auth]\ntype=auth\nusername=phone\npassword=test-password\n[bob-auth]\ntype=auth\nusername=bob\npassword=bob-password\n[charlie-auth]\ntype=auth\nusername=charlie\npassword=charlie-password\n[spoofed-auth]\ntype=auth\nusername=spoofed\npassword=spoofed-password\n[alice-aor]\ntype=aor\n[bob-aor]\ntype=aor\ncontact=sip:bob@{}\n[charlie-aor]\ntype=aor\ncontact=sip:charlie@{}\n[spoofed-aor]\ntype=aor\n[spoofed]\ntype=endpoint\ntransport=udp\nauth=spoofed-auth\naors=spoofed-aor\ncontext=outside\nallow=ulaw\n[alice]\ntype=endpoint\ntransport=udp\nauth=alice-auth\naors=alice-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=bob-auth\naors=bob-aor\ncontext=internal\nallow=ulaw\n[charlie]\ntype=endpoint\ntransport=udp\nauth=charlie-auth\naors=charlie-aor\ncontext=internal\nallow=ulaw\n", bob.contact()?, charlie.contact()?);
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Dial(PJSIP/${EXTEN}@bob,5)\nsame => n,Hangup()\nexten => 200,1,Dial(PJSIP/${EXTEN}@charlie,5)\nsame => n,Hangup()\n[outside]\nexten => 100,1,Hangup()\n",
    ).map_err(failure)?;
    config.rtp.start = 0;
    config.rtp.end = 0;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let mut addresses = Vec::with_capacity(16);
    addresses.push(target);
    for peer in [&alice, &bob, &charlie] {
        addresses.extend(peer.addresses()?);
    }
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let (media_ports, mut server_ports) = mpsc::channel(3);
    let exchange = tokio::spawn(exercise::run(
        [alice, bob, charlie],
        target,
        media_ports,
        departure,
        RequestRoute::Direct(target),
    ));
    let mut result = finish(exchange, Duration::from_secs(8)).await;
    controller.shutdown();
    let limit = Duration::from_secs(2);
    let (signaling, hub) = tokio::join!(finish(signaling, limit), finish(hub, limit));
    result = combine(
        result,
        signaling.map_err(|error| failure(format!("signaling shutdown: {error}"))),
    );
    result = combine(
        result,
        hub.map_err(|error| failure(format!("media shutdown: {error}"))),
    );
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
