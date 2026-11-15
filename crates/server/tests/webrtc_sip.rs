use rsipstack::sip::{HeadersExt, Method, SipMessage};
use rusterisk_api::{Channel, ChannelId, ChannelState, EventKind};
use rusterisk_config::{Codec, Config};
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{Frame, MediaError};
use rusterisk_server::{
    media::MediaHub,
    media_link::Channels,
    rtp::SocketPair,
    signaling::Signaling,
    webrtc::{WebRtcSession, WebRtcStats},
    Action, Controller, Credentials,
};
use std::{
    collections::BTreeSet,
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
};
#[path = "webrtc_sip/direction.rs"]
mod direction;
#[path = "webrtc_sip/incoming.rs"]
mod incoming;

enum CallOrigin {
    Device,
    Controller,
}

enum MediaCycle {
    Duplex,
    HoldResume,
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

struct Application {
    incoming: mpsc::Receiver<Frame>,
    outgoing: mpsc::Sender<Frame>,
    stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
}

fn channels() -> (Channels, Application) {
    let (incoming, input) = mpsc::channel(3);
    let (outgoing, output) = mpsc::channel(3);
    let (digits, _digits) = mpsc::channel(16);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (directed, direction) = watch::channel(PeerMediaDirection::SendReceive);
    (
        Channels {
            incoming,
            digits,
            outgoing: output,
            commands,
            completed,
            stop: stopped,
            direction,
        },
        Application {
            incoming: input,
            outgoing,
            stop,
            _direction: directed,
        },
    )
}

async fn answer(signal: &UdpSocket) -> Result<(WebRtcSession, direction::Dialog), MediaError> {
    let mut bytes = [0; 16385];
    let (size, server) = tokio::time::timeout(Duration::from_secs(2), signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(MediaError::InvalidPacket);
    };
    assert_eq!(request.method, Method::Invite);
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (session, body) = WebRtcSession::answer(
        sockets,
        local,
        std::str::from_utf8(&request.body).map_err(failure)?,
        &[Codec::Pcmu],
        Instant::now(),
    )?;
    let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=phone\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value(), signal.local_addr().map_err(failure)?, body.len());
    let dialog = direction::Dialog::outgoing(&request, &body, server)?;
    signal
        .send_to(response.as_bytes(), server)
        .await
        .map_err(failure)?;
    Ok((session, dialog))
}

async fn finished(
    mut task: tokio::task::JoinHandle<Result<WebRtcStats, MediaError>>,
) -> Result<WebRtcStats, MediaError> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.map_err(failure)?,
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

async fn bridge_calls(
    origin: CallOrigin,
    bind: Ipv4Addr,
    cycle: MediaCycle,
) -> Result<(), MediaError> {
    let peers = [
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
        UdpSocket::bind("127.0.0.1:0").await.map_err(failure)?,
    ];
    let mut sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind={bind}:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n");
    for (name, peer) in ["left", "right"].into_iter().zip(&peers) {
        sip.push_str(&format!("[{name}-aor]\ntype=aor\ncontact=sip:phone@{}\n[{name}]\ntype=endpoint\ntransport=udp\nauth=auth\naors={name}-aor\nallow=ulaw\nwebrtc=yes\ndtmf_mode=info\ncontext=internal\n", peer.local_addr().map_err(failure)?));
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
    let (controller, runner, effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), effects, media, notices)
        .await
        .map_err(failure)?;
    let target = signaling
        .listeners()
        .map_err(failure)?
        .first()
        .ok_or(MediaError::InvalidRoute)?
        .1;
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let mut applications = Vec::with_capacity(2);
    let mut jobs = Vec::with_capacity(2);
    let result = tokio::time::timeout(Duration::from_secs(7), async {
        let credentials = Credentials::new("app".into(), "test-password".into()).map_err(failure)?;
        let mut events = controller.subscribe(credentials.clone(), None, "calls".into()).await.map_err(failure)?;
        let mut ids = Vec::with_capacity(2);
        let mut sessions = Vec::with_capacity(2);
        let mut dialogs = Vec::with_capacity(2);
        for (index, (name, peer)) in ["left", "right"].into_iter().zip(&peers).enumerate() {
            if index == 0 && matches!(origin, CallOrigin::Device) {
                let (id, session, dialog) = incoming::answer_device(peer, target, &controller, &credentials, &mut events.events).await?;
                ids.push(id);
                sessions.push(session);
                dialogs.push(dialog);
                continue;
            }
            let id = ChannelId::try_from(name.to_owned()).map_err(failure)?;
            let accepted = controller.execute(credentials.clone(), None, Action::Originate { id: id.clone(), endpoint: format!("PJSIP/{name}"), app: "calls".into(), args: Vec::new(), caller: "100".into(), timeout: 3 }).await.map_err(failure)?;
            let accepted: Channel = serde_json::from_value(accepted.body.ok_or(MediaError::InvalidRoute)?).map_err(failure)?;
            assert_eq!(accepted.state, ChannelState::Down);
            let (session, dialog) = answer(peer).await?;
            sessions.push(session);
            dialogs.push(dialog);
            loop {
                if let EventKind::StasisStart {channel, ..} = events.events.recv().await.ok_or(MediaError::UnknownStream)?.kind {
                    if channel.id == id { assert_eq!(channel.state, ChannelState::Up); break; }
                }
            }
            ids.push(id);
        }
        let bridge = ChannelId::try_from("browser-bridge".to_owned()).map_err(failure)?;
        controller.execute(credentials.clone(), None, Action::CreateBridge { id: bridge.clone(), name: "WebRTC calls".into() }).await.map_err(failure)?;
        controller.execute(credentials.clone(), None, Action::AddChannels { bridge: bridge.clone(), channels: ids.clone() }).await.map_err(failure)?;
        for session in sessions {
            let (channels, application) = channels();
            jobs.push(tokio::spawn(session.run(channels)));
            applications.push(application);
        }
        let [left, right] = applications.as_mut_slice() else { return Err(MediaError::InvalidRoute); };
        left.incoming.recv().await.ok_or(MediaError::UnknownStream)?;
        right.incoming.recv().await.ok_or(MediaError::UnknownStream)?;
        for id in &ids {
            let value = controller.execute(credentials.clone(), None, Action::GetVariable {channel: id.clone(), variable: "RUSTERISK_MEDIA_CONNECTED".into()}).await.map_err(failure)?;
            assert_eq!(value.body, Some(serde_json::json!({"value":"0"})));
        }
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sent = 0;
        let mut audio = [false; 2];
        let mut connected = BTreeSet::new();
        while sent < 30 || !audio.iter().all(|received| *received) || connected.len() < 2 {
            tokio::select! {
                _instant = clock.tick(), if sent < 30 => {
                    left.outgoing.try_send(Frame::new(&[4000; 960], 48000)?).map_err(failure)?;
                    right.outgoing.try_send(Frame::new(&[-7000; 960], 48000)?).map_err(failure)?;
                    sent += 1;
                }
                frame = left.incoming.recv() => audio[0] |= frame.ok_or(MediaError::UnknownStream)?.samples().iter().all(|sample| (i32::from(*sample) + 7000).abs() < 300),
                frame = right.incoming.recv() => audio[1] |= frame.ok_or(MediaError::UnknownStream)?.samples().iter().all(|sample| (i32::from(*sample) - 4000).abs() < 300),
                event = events.events.recv() => {
                    if let EventKind::ChannelVarset { channel, variable, value } = event.ok_or(MediaError::UnknownStream)?.kind {
                        if variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1" { connected.insert(channel.id); }
                    }
                }
            }
        }
        assert!(ids.iter().all(|id| connected.contains(id)));
        if matches!(cycle, MediaCycle::HoldResume) {
            direction::cycle(&peers[0], &mut dialogs[0], left, right).await?;
        }
        for id in ids { controller.execute(credentials.clone(), None, Action::Hangup(id)).await.map_err(failure)?; }
        controller.execute(credentials, None, Action::DestroyBridge(bridge)).await.map_err(failure)?;
        Ok::<(), MediaError>(())
    }).await.map_err(failure);
    for application in applications {
        application.stop.send_replace(true);
    }
    let mut peer_failure = None;
    for job in jobs {
        match finished(job).await {
            Ok(stats) => {
                assert!(stats.transport_connected);
                assert!(stats.audio.packets_received > 0);
            }
            Err(error) => {
                if peer_failure.is_none() {
                    peer_failure = Some(error);
                }
            }
        }
    }
    controller.shutdown();
    let runner = runner.await.map_err(failure);
    let signaling = signaling.await.map_err(failure);
    let hub = hub.await.map_err(failure);
    result??;
    if let Some(error) = peer_failure {
        return Err(error);
    }
    runner?.map_err(failure)?;
    signaling?.map_err(failure)?;
    hub??;
    Ok(())
}

#[tokio::test]
async fn outgoing_sip_calls_answer_before_received_webrtc_audio_and_bridge_both_directions(
) -> Result<(), MediaError> {
    bridge_calls(
        CallOrigin::Controller,
        Ipv4Addr::LOCALHOST,
        MediaCycle::Duplex,
    )
    .await
}

#[tokio::test]
async fn authenticated_incoming_sip_offers_exchange_received_audio_after_answer(
) -> Result<(), MediaError> {
    bridge_calls(CallOrigin::Device, Ipv4Addr::LOCALHOST, MediaCycle::Duplex).await
}

#[tokio::test]
async fn wildcard_signaling_uses_a_routable_local_media_address() -> Result<(), MediaError> {
    bridge_calls(
        CallOrigin::Controller,
        Ipv4Addr::UNSPECIFIED,
        MediaCycle::Duplex,
    )
    .await
}

#[tokio::test]
async fn outgoing_webrtc_direction_updates_stop_and_resume_received_bridge_audio(
) -> Result<(), MediaError> {
    bridge_calls(
        CallOrigin::Controller,
        Ipv4Addr::LOCALHOST,
        MediaCycle::HoldResume,
    )
    .await
}

#[tokio::test]
async fn incoming_webrtc_direction_updates_stop_and_resume_received_bridge_audio(
) -> Result<(), MediaError> {
    bridge_calls(
        CallOrigin::Device,
        Ipv4Addr::LOCALHOST,
        MediaCycle::HoldResume,
    )
    .await
}
