use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine as Base64Engine;
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    Method, Uri,
};
use rsipstack::sip::{HeadersExt, SipMessage};
use rusterisk_api::{ChannelId, ChannelState, EventKind};
use rusterisk_config::{Codec, Config, Dtmf};
use rusterisk_core::{DtmfDigit, MuteDirection};
use rusterisk_media::{
    mulaw_decode, AudioFormat, Frame, RtpPacket, RtpSender, TelephoneEvent, TelephoneReceiver,
};
use rusterisk_server::media::MediaHub;
use rusterisk_server::rtp::SocketPair;
use rusterisk_server::signaling::Signaling;
use rusterisk_server::{Action, ControlError, Controller, Credentials};
use rusterisk_sip::{Direction, DtmfFormat, Session};
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
#[path = "sip_call/early_music.rs"]
mod early_music;
#[path = "sip_call/late_answer.rs"]
mod late_answer;
#[path = "sip_call/test_probe.rs"]
mod test_probe;
use tokio::net::UdpSocket;
use tower::ServiceExt;

#[path = "sip_transfer/acknowledgements.rs"]
mod acknowledgements;
#[path = "sip_transfer/attended.rs"]
mod attended_transfer;
#[path = "sip_transfer/blind.rs"]
mod blind_transfer;
#[path = "sip_transfer/lifetime.rs"]
mod transfer_lifetime;
#[path = "sip_transfer/peer.rs"]
mod transfer_peer;
#[path = "sip_transfer/reference_configuration.rs"]
mod transfer_reference_configuration;
#[path = "sip_transfer/signals.rs"]
mod transfer_signals;
#[path = "sip_transfer/transaction_tests.rs"]
mod transfer_transaction_tests;
#[path = "sip_transfer/transaction_identity.rs"]
mod transfer_transactions;
use acknowledgements::AckIdentity;
use transfer_transactions::BranchNamespace;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}
fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}
fn id(value: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(value.to_owned())?)
}

async fn matching_event(
    events: &mut tokio::sync::mpsc::Receiver<rusterisk_api::Event>,
    matches: impl Fn(&EventKind) -> bool,
) -> Result<EventKind, ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.recv().await {
            if matches(&event.kind) {
                return Ok(event.kind);
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

async fn answer(signal: &UdpSocket, audio: &SocketPair) -> Result<SocketAddr, ControlError> {
    answer_dialog(signal, audio)
        .await
        .map(|(address, _ack)| address)
}

async fn answer_dialog(
    signal: &UdpSocket,
    audio: &SocketPair,
) -> Result<(SocketAddr, String), ControlError> {
    answer_dialog_at(
        signal,
        signal.local_addr().map_err(failure)?,
        audio.local_addr().map_err(failure)?,
        None,
    )
    .await
    .map(|(media, ack, _identity)| (media, ack))
}

async fn answer_dialog_at(
    signal: &UdpSocket,
    contact: SocketAddr,
    audio: SocketAddr,
    expected_user: Option<&str>,
) -> Result<(SocketAddr, String, AckIdentity), ControlError> {
    use rsipstack::sip::headers::untyped::ToTypedHeader;

    let mut bytes = [0; 16385];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(2), signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let request = match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(ControlError::Invalid("Expected an invitation")),
    };
    if let Some(user) = expected_user {
        assert_eq!(
            request.uri.auth.as_ref().map(|auth| auth.user.as_str()),
            Some(user),
            "INVITE Request-URI"
        );
    }
    let tagged_to = format!(
        "{};tag=phone",
        request.to_header().map_err(failure)?.value()
    );
    let acknowledgement = AckIdentity::for_invitation(&request, peer, tagged_to.clone())?;
    let offered = Session::parse(
        std::str::from_utf8(&request.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?;
    let body = offered.answer(audio, 1).map_err(failure)?;
    let reply = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {tagged_to}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:phone@{contact}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value(), body.len());
    signal
        .send_to(reply.as_bytes(), peer)
        .await
        .map_err(failure)?;
    let branch = BranchNamespace::random()?.branch(0);
    let remote = request
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let ack = format!("ACK {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch={branch};rport\r\nFrom: {};tag=phone\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", signal.local_addr().map_err(failure)?, request.to_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value());
    Ok((offered.remote, ack, acknowledgement))
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

fn drain_received_audio(peer: &SocketPair) -> Result<(), ControlError> {
    let mut bytes = [0; 1501];
    for _ in 0..128 {
        match peer.data().try_recv_from(&mut bytes) {
            Ok((size, _)) => {
                RtpPacket::parse(&bytes[..size]).map_err(failure)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(failure(error)),
        }
    }
    Err(ControlError::Capacity)
}

async fn send_voice(
    peer: &SocketPair,
    target: SocketAddr,
    sender: &mut RtpSender,
    value: i16,
) -> Result<(), ControlError> {
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for _ in 0..5 {
        clock.tick().await;
        let packet = sender
            .packet(Frame::new(&[value; 160], 8000).map_err(failure)?)
            .map_err(failure)?
            .encode()
            .map_err(failure)?;
        peer.data()
            .send_to(&packet, target)
            .await
            .map_err(failure)?;
    }
    Ok(())
}

async fn update_direction(
    signal: &UdpSocket,
    target: SocketAddr,
    ack: &str,
    audio: &SocketPair,
    sequence: u32,
    version: u64,
    direction: &str,
) -> Result<(), ControlError> {
    use rsipstack::sip::headers::untyped::ToTypedHeader;

    let body = Session {
        remote: audio.local_addr().map_err(failure)?,
        codec: Codec::Pcmu,
        payload_type: 0,
        telephone: Some(DtmfFormat::new(101, 8000).map_err(failure)?),
        direction: Direction::SendRecv,
        security: rusterisk_sip::SessionSecurity::Rtp,
    }
    .answer(audio.local_addr().map_err(failure)?, 1)
    .map_err(failure)?
    .replace("o=rusterisk 1 1 IN", &format!("o=rusterisk 1 {version} IN"))
    .replace("a=sendrecv", &format!("a={direction}"));
    let SipMessage::Request(mut template) = SipMessage::try_from(ack).map_err(failure)? else {
        return Err(ControlError::Invalid("Expected direction acknowledgement"));
    };
    let transactions = BranchNamespace::random()?;
    let mut packet = |transaction| {
        let via = template
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?
            .with_branch(transactions.branch(transaction).to_string().into());
        *template.via_header_mut().map_err(failure)? = via.into();
        Ok::<_, ControlError>(template.to_string())
    };
    let offer = packet(0)?.replace("ACK sip:", "INVITE sip:")
        .replace("CSeq: 2 ACK", &format!("CSeq: {sequence} INVITE"))
        .replace("Content-Length: 0\r\n\r\n", &format!("Contact: <sip:phone@{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", signal.local_addr().map_err(failure)?, body.len()));
    signal
        .send_to(offer.as_bytes(), target)
        .await
        .map_err(failure)?;
    assert_eq!(response(signal).await?.status_code.code(), 200);
    let ack = packet(1)?.replace("CSeq: 2 ACK", &format!("CSeq: {sequence} ACK"));
    signal
        .send_to(ack.as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

async fn receive_digit(
    events: &mut tokio::sync::mpsc::Receiver<rusterisk_api::Event>,
    channel: &ChannelId,
    expected: &str,
) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.recv().await {
            if let EventKind::ChannelDtmfReceived {
                channel: actual,
                digit,
                duration_ms,
            } = event.kind
            {
                assert_eq!(&actual.id, channel);
                assert_eq!(digit, expected);
                assert_eq!(duration_ms, 120);
                return Ok(());
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

async fn receive_outgoing_digits(
    audio: &SocketPair,
    signaling: &UdpSocket,
) -> Result<(), ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let received_audio = async {
            let mut receiver = TelephoneReceiver::new(101, 8000).map_err(failure)?;
            let mut bytes = [0; 1501];
            let mut digits = String::new();
            let mut ends = 0;
            for now in 0..100 {
                let (size, _) = audio.data().recv_from(&mut bytes).await.map_err(failure)?;
                let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
                if packet.payload_type() == 101 {
                    if TelephoneEvent::parse(packet.payload()).map_err(failure)?.ended() { ends += 1; }
                    for tone in receiver.push(&packet, now).map_err(failure)?.into_tones() {
                        assert_eq!(tone.duration_ms, 60);
                        digits.push(tone.digit.symbol());
                    }
                }
                if digits.len() == 2 && ends == 6 { assert_eq!(digits, "11"); return Ok(()); }
            }
            Err(ControlError::Capacity)
        };
        let received_info = async {
            let mut bytes = [0; 16385];
            let mut digits = String::new();
            for _ in 0..8 {
                let (size, peer) = signaling.recv_from(&mut bytes).await.map_err(failure)?;
                let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)? else { return Err(ControlError::Capacity); };
                if request.method == Method::Ack { continue; }
                assert_eq!(request.method, Method::Info);
                let body = std::str::from_utf8(&request.body).map_err(failure)?;
                assert!(body.contains("Duration=60\r\n"));
                let digit = body.strip_prefix("Signal=").and_then(|value| value.chars().next()).ok_or(ControlError::Capacity)?;
                digits.push(digit);
                let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
                signaling.send_to(response.as_bytes(), peer).await.map_err(failure)?;
                if digits.len() == 2 { assert_eq!(digits, "7#"); return Ok(()); }
            }
            Err(ControlError::Capacity)
        };
        tokio::try_join!(received_audio, received_info)?;
        Ok(())
    }).await.map_err(failure)?
}

async fn send_digits(
    controller: &Controller,
    channel: &ChannelId,
    digits: &str,
    between: u32,
    duration: u32,
) -> Result<(), ControlError> {
    let query = serde_urlencoded::to_string([
        ("dtmf", digits.to_owned()),
        ("between", between.to_string()),
        ("duration", duration.to_string()),
    ])
    .map_err(failure)?;
    let request = Request::builder()
        .method("POST")
        .uri(format!("/ari/channels/{channel}/dtmf?{query}"))
        .header(
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("app:test-password")
            ),
        )
        .body(Body::empty())
        .map_err(failure)?;
    let response = rusterisk_server::http::router(controller.clone())
        .oneshot(request)
        .await
        .map_err(failure)?;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    Ok(())
}

async fn receive_bulk_digits(audio: &SocketPair) -> Result<(), ControlError> {
    let mut receiver = TelephoneReceiver::new(101, 8000).map_err(failure)?;
    let mut bytes = [0; 1501];
    let mut received = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    for now in 0..1000 {
        let packet = tokio::time::timeout_at(deadline, audio.data().recv_from(&mut bytes)).await;
        let (size, _) = match packet {
            Ok(result) => result.map_err(failure)?,
            Err(_) => {
                return Err(ControlError::Adapter(format!(
                    "Expected 64 short digits, received {received}"
                )))
            }
        };
        let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
        if packet.payload_type() == 101 {
            for tone in receiver.push(&packet, now).map_err(failure)?.into_tones() {
                assert_eq!(tone.digit, DtmfDigit::One);
                assert_eq!(tone.duration_ms, 1);
                received += 1;
            }
        }
        if received == 64 {
            return Ok(());
        }
    }
    Err(ControlError::Adapter(format!(
        "Expected 64 short digits, received {received}"
    )))
}

struct AudioPeer<'a> {
    socket: &'a SocketPair,
    target: SocketAddr,
    sender: RtpSender,
    sample: i16,
}

impl AudioPeer<'_> {
    async fn send(&mut self) -> Result<(), ControlError> {
        let packet = self
            .sender
            .packet(Frame::new(&[self.sample; 160], 8000).map_err(failure)?)
            .map_err(failure)?
            .encode()
            .map_err(failure)?;
        self.socket
            .data()
            .send_to(&packet, self.target)
            .await
            .map_err(failure)?;
        Ok(())
    }
}

async fn stable_audio(peer: &SocketPair, expected: i32) -> Result<(), ControlError> {
    let mut bytes = [0; 1501];
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut consecutive = 0;
        for _ in 0..100 {
            let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
            let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
            if packet.payload().len() == 160
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
        Err(ControlError::Invalid(
            "Received audio did not stabilize at the expected signal",
        ))
    })
    .await
    .map_err(failure)?
}

async fn exchange(
    left: &mut AudioPeer<'_>,
    right: &mut AudioPeer<'_>,
    expected: (i32, i32),
) -> Result<(), ControlError> {
    let first = left.socket;
    let second = right.socket;
    for peer in [first, second] {
        let mut bytes = [0; 1501];
        for _ in 0..100 {
            match peer.data().try_recv_from(&mut bytes) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(failure(error)),
            }
        }
    }
    let transmit = async {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..100 {
            tick.tick().await;
            left.send().await?;
            right.send().await?;
        }
        Err::<(), ControlError>(ControlError::Invalid(
            "Audio verification exceeded its packet bound",
        ))
    };
    let receive = async {
        tokio::try_join!(
            stable_audio(first, expected.0),
            stable_audio(second, expected.1)
        )?;
        Ok::<(), ControlError>(())
    };
    tokio::select! {
        result = transmit => result?,
        result = receive => result?,
    }
    Ok(())
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
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let signal_stop = controller.shutdown_signal();
    let media_stop = controller.shutdown_signal();
    let hub = tokio::spawn(hub.run(media_stop));
    let signaling = tokio::spawn(signaling.run(signal_stop));
    let result = async {
        let mut events = controller
            .subscribe(credentials()?, None, "app".into())
            .await?;
        for (name, destination) in [("alice", "alice"), ("bob", "+15550123@bob")] {
            controller
                .execute(
                    credentials()?,
                    None,
                    Action::Originate {
                        id: id(name)?,
                        endpoint: format!("PJSIP/{destination}"),
                        app: "app".into(),
                        args: Vec::new(),
                        caller: "100".into(),
                        timeout: 30,
                    },
                )
                .await?;
        }
        let mut packet = [0; 4096];
        let (size, _) = tokio::time::timeout(Duration::from_secs(2), bob.peek_from(&mut packet))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let SipMessage::Request(invite) = SipMessage::try_from(&packet[..size]).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected carrier invitation"));
        };
        assert_eq!(
            invite.uri.to_string(),
            format!("sip:+15550123@{}", bob.local_addr().map_err(failure)?)
        );
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
        let mut left = AudioPeer {
            socket: &alice_audio,
            target: alice_target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?,
            sample: 4000,
        };
        let mut right = AudioPeer {
            socket: &bob_audio,
            target: bob_target,
            sender: RtpSender::new(AudioFormat::Pcmu, 0, [9; 10]).map_err(failure)?,
            sample: -7000,
        };
        exchange(&mut left, &mut right, (-7000, 4000)).await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::Mute {
                    channel: id("alice")?,
                    direction: MuteDirection::Incoming,
                },
            )
            .await?;
        exchange(&mut left, &mut right, (-7000, 0)).await?;
        controller
            .execute(credentials()?, None, Action::Hold(id("alice")?))
            .await?;
        exchange(&mut left, &mut right, (0, 0)).await?;
        controller
            .execute(credentials()?, None, Action::Unhold(id("alice")?))
            .await?;
        exchange(&mut left, &mut right, (-7000, 0)).await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::Unmute {
                    channel: id("alice")?,
                    direction: MuteDirection::Incoming,
                },
            )
            .await?;
        exchange(&mut left, &mut right, (-7000, 4000)).await?;
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
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}

async fn response(signal: &UdpSocket) -> Result<rsipstack::sip::Response, ControlError> {
    let mut bytes = [0; 16385];
    let (size, _) = tokio::time::timeout(Duration::from_secs(2), signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected a signaling response")),
    }
}

async fn invite(
    signal: &UdpSocket,
    target: SocketAddr,
    audio: &SocketPair,
) -> Result<(), ControlError> {
    invite_at(
        signal,
        target,
        signal.local_addr().map_err(failure)?,
        audio.local_addr().map_err(failure)?,
    )
    .await?;
    if response(signal).await?.status_code.code() != 100 {
        return Err(ControlError::Invalid(
            "Missing authenticated invitation progress",
        ));
    }
    Ok(())
}

async fn final_response(signal: &UdpSocket) -> Result<rsipstack::sip::Response, ControlError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..8 {
            let response = response(signal).await?;
            if response.status_code.code() >= 200 {
                return Ok(response);
            }
        }
        Err(ControlError::Capacity)
    })
    .await
    .map_err(failure)?
}

async fn invite_at(
    signal: &UdpSocket,
    target: SocketAddr,
    local: SocketAddr,
    audio: SocketAddr,
) -> Result<(), ControlError> {
    start_invitation(
        signal,
        target,
        local,
        audio,
        Invitation {
            extension: "100",
            call_id: "incoming-test",
            from_tag: "caller",
        },
    )
    .await
}

struct Invitation<'a> {
    extension: &'a str,
    call_id: &'a str,
    from_tag: &'a str,
}

async fn start_invitation(
    signal: &UdpSocket,
    target: SocketAddr,
    local: SocketAddr,
    audio: SocketAddr,
    invitation: Invitation<'_>,
) -> Result<(), ControlError> {
    let Invitation {
        extension,
        call_id,
        from_tag,
    } = invitation;
    let uri = Uri::try_from(format!("sip:{extension}@{target}")).map_err(failure)?;
    let body = Session {
        remote: audio,
        codec: Codec::Pcmu,
        payload_type: 0,
        telephone: Some(DtmfFormat::new(101, 8000).map_err(failure)?),
        direction: Direction::SendRecv,
        security: rusterisk_sip::SessionSecurity::Rtp,
    }
    .answer(audio, 1)
    .map_err(failure)?;
    let transactions = BranchNamespace::random()?;
    let packet = |sequence: u32, authorization: &str| {
        let branch = transactions.branch(sequence);
        format!("INVITE {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch={branch};rport\r\nFrom: <sip:spoofed@localhost>;tag={from_tag}\r\nTo: <{uri}>\r\nCall-ID: {call_id}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@{local}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    };
    signal
        .send_to(packet(1, "").as_bytes(), target)
        .await
        .map_err(failure)?;
    let challenge = final_response(signal).await?;
    if challenge.status_code.code() != 401 {
        return Err(ControlError::Invalid(
            "Missing invitation authentication challenge",
        ));
    }
    let branch = transactions.branch(1);
    let ack = format!("ACK {uri} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 1 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", challenge.from_header().map_err(failure)?.value(), challenge.to_header().map_err(failure)?.value(), challenge.call_id_header().map_err(failure)?.value());
    signal
        .send_to(ack.as_bytes(), target)
        .await
        .map_err(failure)?;
    let challenge = WwwAuthenticate::parse(
        challenge
            .www_authenticate_header()
            .ok_or(ControlError::Capacity)?
            .value(),
    )
    .map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "test-client".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "phone",
        password: "test-password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"test-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
    signal
        .send_to(packet(2, &authorization).as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn authenticated_incoming_calls_enter_stasis_before_answer_and_exchange_audio_with_an_outgoing_peer(
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
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[alice-contact]\ntype=aor\n[bob-contact]\ntype=aor\ncontact=sip:bob@{}\n[alice]\ntype=endpoint\ntransport=udp\nauth=auth\naors=alice-contact\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=auth\naors=bob-contact\nallow=ulaw\n", bob.local_addr().map_err(failure)?);
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => _.,1,NoOp(start)\nsame => n,Set(LABEL=root-${EXTEN}-${CALLERID(num)})\nsame => n,Wait(0)\nsame => n,Stasis(app,${LABEL})\n",
    )
    .map_err(failure)?;
    let mut config = config;
    config
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.name == "bob")
        .ok_or(ControlError::Capacity)?
        .dtmf = Dtmf::Info;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        let mut events = controller.subscribe(credentials()?, None, "app".into()).await?;
        invite(&alice, target, &alice_audio).await?;
        let incoming = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = events.events.recv().await {
                if let EventKind::StasisStart { channel, args } = event.kind {
                    assert_eq!(channel.state, ChannelState::Ring);
                    assert_eq!(channel.caller.number, "spoofed");
                    assert!(channel.name.starts_with("PJSIP/alice-"));
                    assert_eq!(args, ["root-100-spoofed"]);
                    return Ok(channel.id);
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        controller.execute(credentials()?, None, Action::Ring(incoming.clone())).await?;
        assert_eq!(response(&alice).await?.status_code.code(), 180);
        controller.execute(credentials()?, None, Action::Answer(incoming.clone())).await?;
        let answered = response(&alice).await?;
        assert_eq!(answered.status_code.code(), 200);
        let alice_target = Session::parse(std::str::from_utf8(&answered.body).map_err(failure)?, &[Codec::Pcmu]).map_err(failure)?.remote;
        let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
        alice.send_to(ack.as_bytes(), target).await.map_err(failure)?;
        controller.execute(credentials()?, None, Action::Originate { id: id("bob")?, endpoint: "PJSIP/bob".into(), app: "app".into(), args: Vec::new(), caller: "100".into(), timeout: 30 }).await?;
        let (bob_target, bob_ack) = answer_dialog(&bob, &bob_audio).await?;
        let mut up = BTreeSet::new();
        while up.len() < 2 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
            if let EventKind::ChannelStateChange { channel } = event.kind {
                if channel.state == ChannelState::Up { up.insert(channel.id); }
            }
        }
        let body = "Signal=#\r\nDuration=120\r\n";
        for (channel, digits) in [(incoming.clone(), "11"), (id("bob")?, "7#")] {
            send_digits(&controller, &channel, digits, 20, 60).await?;
        }
        receive_outgoing_digits(&alice_audio, &bob).await?;
        let info = ack.replace("ACK sip:", "INFO sip:")
            .replace("z9hG4bK-ack", "z9hG4bK-info")
            .replace("CSeq: 2 ACK", "CSeq: 3 INFO")
            .replace("Content-Length: 0\r\n\r\n", &format!("Content-Type: application/dtmf-relay\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
        for _ in 0..3 {
            alice.send_to(info.as_bytes(), target).await.map_err(failure)?;
            assert_eq!(response(&alice).await?.status_code.code(), 200);
        }
        receive_digit(&mut events.events, &incoming, "#").await?;
        for sequence in 1..=3 {
            let payload = TelephoneEvent::new(DtmfDigit::Five, true, 10, 960).map_err(failure)?.encode();
            let packet = RtpPacket::new(101, sequence, 1000, 0x07070707, &payload).map_err(failure)?.encode().map_err(failure)?;
            alice_audio.data().send_to(&packet, alice_target).await.map_err(failure)?;
        }
        receive_digit(&mut events.events, &incoming, "5").await?;
        controller.execute(credentials()?, None, Action::CreateBridge { id: id("mix")?, name: "Conversation".into() }).await?;
        controller.execute(credentials()?, None, Action::AddChannels { bridge: id("mix")?, channels: [incoming.clone(), id("bob")?].into_iter().collect() }).await?;
        let mut alice_sender = RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?;
        let mut bob_sender = RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?;
        tokio::try_join!(send_voice(&alice_audio, alice_target, &mut alice_sender, 4000), send_voice(&bob_audio, bob_target, &mut bob_sender, -7000))?;
        tokio::try_join!(receive_audio(&alice_audio, -7000), receive_audio(&bob_audio, 4000))?;
        for (sequence, version, direction, received_by_bob) in [(4, 2, "sendonly", 4000), (5, 3, "inactive", 0), (6, 4, "sendrecv", 0)] {
            if direction == "sendrecv" {
                controller.execute(credentials()?, None, Action::Mute { channel: incoming.clone(), direction: MuteDirection::Incoming }).await?;
            }
            update_direction(&alice, target, &ack, &alice_audio, sequence, version, direction).await?;
            if direction == "sendonly" {
                matching_event(&mut events.events, |event| matches!(event, EventKind::ChannelHold { channel, musicclass: Some(class) } if channel.id == incoming && class == "default")).await?;
            } else if direction == "sendrecv" {
                matching_event(&mut events.events, |event| matches!(event, EventKind::ChannelUnhold { channel } if channel.id == incoming)).await?;
            }
            drain_received_audio(&alice_audio)?;
            drain_received_audio(&bob_audio)?;
            tokio::try_join!(send_voice(&alice_audio, alice_target, &mut alice_sender, 4000), send_voice(&bob_audio, bob_target, &mut bob_sender, -7000))?;
            receive_audio(&bob_audio, received_by_bob).await?;
            if direction == "sendrecv" {
                receive_audio(&alice_audio, -7000).await?;
            } else {
                let mut bytes = [0; 1501];
                assert!(tokio::time::timeout(Duration::from_millis(150), alice_audio.data().recv_from(&mut bytes)).await.is_err());
            }
        }
        controller.execute(credentials()?, None, Action::Unmute { channel: incoming.clone(), direction: MuteDirection::Incoming }).await?;
        drain_received_audio(&bob_audio)?;
        send_voice(&alice_audio, alice_target, &mut alice_sender, 4000).await?;
        receive_audio(&bob_audio, 4000).await?;
        for (sequence, version, direction) in [(1, 2, "sendonly"), (2, 3, "sendrecv")] {
            update_direction(&bob, target, &bob_ack, &bob_audio, sequence, version, direction).await?;
            matching_event(&mut events.events, |event| match direction {
                "sendonly" => matches!(event, EventKind::ChannelHold { channel, .. } if channel.id.as_str() == "bob"),
                _ => matches!(event, EventKind::ChannelUnhold { channel } if channel.id.as_str() == "bob"),
            }).await?;
            drain_received_audio(&alice_audio)?;
            drain_received_audio(&bob_audio)?;
            tokio::try_join!(send_voice(&alice_audio, alice_target, &mut alice_sender, 4000), send_voice(&bob_audio, bob_target, &mut bob_sender, -7000))?;
            receive_audio(&alice_audio, -7000).await?;
            if direction == "sendonly" {
                let mut bytes = [0; 1501];
                assert!(tokio::time::timeout(Duration::from_millis(150), bob_audio.data().recv_from(&mut bytes)).await.is_err());
            } else {
                receive_audio(&bob_audio, 4000).await?;
            }
        }
        for _ in 0..2 {
            send_digits(&controller, &incoming, "11111111111111111111111111111111", 0, 1).await?;
        }
        receive_bulk_digits(&alice_audio).await?;
        controller.execute(credentials()?, None, Action::Hangup(incoming)).await?;
        controller.execute(credentials()?, None, Action::Hangup(id("bob")?)).await?;
        Ok(())
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}

#[tokio::test]
async fn disconnecting_the_stasis_app_rejects_an_unanswered_invitation() -> Result<(), ControlError>
{
    let phone = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let audio = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
    let sip = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[contact]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=contact\ncontext=internal\nallow=ulaw\n";
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        sip,
        "",
        "[internal]\nexten => 100,1,Stasis(app)\n",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        let mut subscription = controller
            .subscribe(credentials()?, None, "app".into())
            .await?;
        invite(&phone, target, &audio).await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = subscription.events.recv().await {
                if matches!(event.kind, EventKind::StasisStart { .. }) {
                    return Ok(());
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)??;
        drop(subscription);
        assert_eq!(response(&phone).await?.status_code.code(), 603);
        let channels = controller
            .execute(credentials()?, None, Action::ListChannels)
            .await?
            .body
            .ok_or(ControlError::Capacity)?;
        assert_eq!(channels, serde_json::json!([]));
        Ok(())
    }
    .await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}

#[tokio::test]
async fn dialplan_answer_wait_and_hangup_finish_a_real_invitation_without_stasis(
) -> Result<(), ControlError> {
    let phone = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let audio = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
    let sip = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[contact]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=contact\ncontext=internal\nallow=ulaw\n";
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        sip,
        "",
        "[internal]\nexten => 100,1,Answer()\nsame => n,Wait(1)\nsame => n,Hangup()\n",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        invite(&phone, target, &audio).await?;
        let answered = response(&phone).await?;
        assert_eq!(answered.status_code.code(), 200);
        let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-answer-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", phone.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
        phone.send_to(ack.as_bytes(), target).await.map_err(failure)?;
        let channels = controller.execute(credentials()?, None, Action::ListChannels).await?.body.ok_or(ControlError::Capacity)?;
        assert_eq!(channels[0]["state"], "Up");
        assert_eq!(channels[0]["dialplan"]["app_name"], "Answer");
        let mut bytes = [0; 16385];
        let (size, peer) = tokio::time::timeout(Duration::from_secs(3), phone.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
        let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)? else { return Err(ControlError::Invalid("Expected hangup request")); };
        assert_eq!(request.method, Method::Bye);
        let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
        phone.send_to(response.as_bytes(), peer).await.map_err(failure)?;
        let channels = controller.execute(credentials()?, None, Action::ListChannels).await?.body.ok_or(ControlError::Capacity)?;
        assert_eq!(channels, serde_json::json!([]));
        Ok(())
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}

#[tokio::test]
async fn dialplan_and_http_playback_deliver_sound_and_recording_audio_before_completion(
) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut recording =
        rusterisk_storage::Recorder::create(directory.path(), "welcome", 8000).map_err(failure)?;
    for _ in 0..20 {
        recording
            .write(Frame::new(&[5000; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    recording.finish().map_err(failure)?;
    std::fs::create_dir(directory.path().join("sounds")).map_err(failure)?;
    std::fs::rename(
        directory.path().join("recordings/welcome.wav"),
        directory.path().join("sounds/welcome.wav"),
    )
    .map_err(failure)?;
    let mut beep =
        rusterisk_storage::Recorder::create(directory.path(), "beep", 8000).map_err(failure)?;
    for _ in 0..10 {
        beep.write(Frame::new(&[3000; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    beep.finish().map_err(failure)?;
    std::fs::rename(
        directory.path().join("recordings/beep.wav"),
        directory.path().join("sounds/beep.wav"),
    )
    .map_err(failure)?;
    let mut saved =
        rusterisk_storage::Recorder::create(directory.path(), "saved", 8000).map_err(failure)?;
    for _ in 0..20 {
        saved
            .write(Frame::new(&[-6000; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    saved.finish().map_err(failure)?;
    let (storage, worker) =
        rusterisk_server::storage::StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
    let phone = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let audio = SocketPair::bind(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, 0).map_err(failure)?;
    let sip = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[contact]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=contact\ncontext=internal\nallow=ulaw\n";
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        sip,
        "",
        "[internal]\nexten => 100,1,Playback(missing)\nsame => n,Playback(welcome)\nsame => n,Stasis(app,${PLAYBACKSTATUS})\n",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices)
        .await?
        .with_storage(storage);
    let target = signaling.listeners()?[0].1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        let mut subscription = controller.subscribe(credentials()?, None, "app".into()).await?;
        invite(&phone, target, &audio).await?;
        let answered = response(&phone).await?;
        assert_eq!(answered.status_code.code(), 200);
        let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-playback-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", phone.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
        phone.send_to(ack.as_bytes(), target).await.map_err(failure)?;
        let mut bytes = [0; 1501];
        let mut consecutive = 0;
        tokio::time::timeout(Duration::from_secs(3), async {
            for _ in 0..150 {
                let (size, _) = audio.data().recv_from(&mut bytes).await.map_err(failure)?;
                let packet = RtpPacket::parse(&bytes[..size]).map_err(failure)?;
                if packet.payload().iter().all(|sample| (i32::from(mulaw_decode(*sample)) - 5000).abs() < 300) {
                    consecutive += 1;
                    if consecutive == 6 { return Ok(()); }
                } else { consecutive = 0; }
            }
            Err(ControlError::Invalid("Expected playback audio"))
        }).await.map_err(failure)??;
        let channel = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = subscription.events.recv().await {
                if let EventKind::StasisStart { channel, args } = event.kind {
                    assert_eq!(args, ["SUCCESS"]);
                    return Ok(channel.id);
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        let app = rusterisk_server::http::router(controller.clone());
        for (name, source, expected) in [("saved-play", "recording:saved", -6000), ("welcome-play", "sound:welcome", 5000)] {
            let request = Request::builder().method("POST").uri(format!("/ari/channels/{channel}/play/{name}?media={source}"))
                .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
            let accepted = app.clone().oneshot(request).await.map_err(failure)?;
            assert_eq!(accepted.status(), StatusCode::CREATED);
            let started = tokio::time::timeout(Duration::from_secs(2), subscription.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
            assert!(matches!(started.kind, EventKind::PlaybackStarted { playback } if playback.id.as_str() == name && playback.media_uri == source));
            receive_audio(&audio, expected).await?;
            let finished = tokio::time::timeout(Duration::from_secs(2), subscription.events.recv()).await.map_err(failure)?.ok_or(ControlError::Capacity)?;
            assert!(matches!(finished.kind, EventKind::PlaybackFinished { playback } if playback.id.as_str() == name && playback.state == rusterisk_api::PlaybackState::Done));
        }
        let destination = Session::parse(std::str::from_utf8(&answered.body).map_err(failure)?, &[Codec::Pcmu]).map_err(failure)?.remote;
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [19; 10]).map_err(failure)?;
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum StopRecordingWith { Info, Rtp, Silence, Control }
        for (mode, name) in [(StopRecordingWith::Info, "received-info"), (StopRecordingWith::Rtp, "received-rtp"), (StopRecordingWith::Silence, "received-silence"), (StopRecordingWith::Control, "received-beep")] {
        let options = match mode { StopRecordingWith::Info | StopRecordingWith::Rtp => "terminateOn=%23", StopRecordingWith::Silence => "maxSilenceSeconds=1", StopRecordingWith::Control => "beep=true" };
        let request = Request::builder().method("POST").uri(format!("/ari/channels/{channel}/record?name={name}&format=wav&{options}"))
            .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
        assert_eq!(app.clone().oneshot(request).await.map_err(failure)?.status(), StatusCode::CREATED);
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = subscription.events.recv().await {
                if let EventKind::RecordingStarted { recording } = event.kind {
                    assert_eq!(recording.name, name);
                    return Ok(());
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        if mode == StopRecordingWith::Control {
            receive_audio(&audio, 3000).await?;
            receive_audio(&audio, 0).await?;
        }
        let mut period = tokio::time::interval(Duration::from_millis(20));
        period.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..20 {
            period.tick().await;
            let packet = sender.packet(Frame::new(&[1234; 160], 8000).map_err(failure)?).map_err(failure)?.encode().map_err(failure)?;
            audio.data().send_to(&packet, destination).await.map_err(failure)?;
            drain_received_audio(&audio)?;
        }
        match mode {
        StopRecordingWith::Info => {
            let body = "Signal=#\r\nDuration=120\r\n";
            let info = ack.replace("ACK sip:", "INFO sip:")
                .replace("z9hG4bK-playback-ack", "z9hG4bK-recording-info")
                .replace("CSeq: 2 ACK", "CSeq: 3 INFO")
                .replace("Content-Length: 0\r\n\r\n", &format!("Content-Type: application/dtmf-relay\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
            phone.send_to(info.as_bytes(), target).await.map_err(failure)?;
            assert_eq!(response(&phone).await?.status_code.code(), 200);
        }
        StopRecordingWith::Rtp => {
            for sequence in 1..=3 {
                let event = TelephoneEvent::new(DtmfDigit::Hash, true, 10, 960).map_err(failure)?.encode();
                let packet = RtpPacket::new(101, sequence, 1000, 0x19191919, &event).map_err(failure)?.encode().map_err(failure)?;
                audio.data().send_to(&packet, destination).await.map_err(failure)?;
            }
        }
        StopRecordingWith::Silence => {
            for _ in 0..60 {
                period.tick().await;
                let packet = sender.packet(Frame::new(&[0; 160], 8000).map_err(failure)?).map_err(failure)?.encode().map_err(failure)?;
                audio.data().send_to(&packet, destination).await.map_err(failure)?;
                drain_received_audio(&audio)?;
            }
        }
        StopRecordingWith::Control => {
            let request = Request::builder().method("POST").uri(format!("/ari/recordings/live/{name}/stop"))
                .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
            assert_eq!(app.clone().oneshot(request).await.map_err(failure)?.status(), StatusCode::NO_CONTENT);
        }
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = subscription.events.recv().await {
                if let EventKind::RecordingFinished { recording } = event.kind {
                    assert_eq!(recording.name, name);
                    assert_eq!(recording.state, rusterisk_api::RecordingState::Done);
                    return Ok(());
                }
            }
            Err(ControlError::Capacity)
        }).await.map_err(failure)??;
        let mut saved = rusterisk_storage::WaveReader::open(&directory.path().join(format!("recordings/{name}.wav"))).map_err(failure)?;
        let mut consecutive = 0;
        let mut longest = 0;
        let mut silent_tail = 0;
        while let Some(frame) = saved.read_frame().map_err(failure)? {
            if mode == StopRecordingWith::Control {
                assert!(frame.samples().iter().all(|sample| (i32::from(*sample) - 1234).abs() < 300));
            }
            consecutive = if frame.samples().iter().all(|sample| (i32::from(*sample) - 1234).abs() < 300) { consecutive + 1 } else { 0 };
            longest = longest.max(consecutive);
            silent_tail = if frame.samples().iter().all(|sample| sample.unsigned_abs() < 128) { silent_tail + 1 } else { 0 };
        }
        assert!(longest >= 6);
        if mode == StopRecordingWith::Silence { assert_eq!(silent_tail, 10); }
        }
        std::fs::remove_file(directory.path().join("sounds/beep.wav")).map_err(failure)?;
        let request = Request::builder().method("POST").uri(format!("/ari/channels/{channel}/record?name=missing-prompt&format=wav&beep=true"))
            .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
        assert_eq!(app.clone().oneshot(request).await.map_err(failure)?.status(), StatusCode::CREATED);
        matching_event(&mut subscription.events, |kind| matches!(kind, EventKind::RecordingStarted { recording } if recording.name == "missing-prompt")).await?;
        let finished = matching_event(&mut subscription.events, |kind| matches!(kind, EventKind::RecordingFinished { recording } if recording.name == "missing-prompt")).await?;
        assert!(matches!(finished, EventKind::RecordingFinished { recording } if recording.state == rusterisk_api::RecordingState::Done));
        assert!(!directory.path().join("recordings/missing-prompt.wav").exists());
        assert!(!directory.path().join("recordings/missing-prompt.partial").exists());
        controller.execute(credentials()?, None, Action::CreateBridge { id: id("playback-mix")?, name: String::new() }).await?;
        controller.execute(credentials()?, None, Action::AddChannels { bridge: id("playback-mix")?, channels: [channel.clone()].into_iter().collect() }).await?;
        matching_event(&mut subscription.events, |kind| matches!(kind, EventKind::ChannelEnteredBridge { bridge, .. } if bridge.id.as_str() == "playback-mix")).await?;
        let request = Request::builder().method("POST").uri("/ari/bridges/playback-mix/play/bridge-play?media=recording:saved")
            .header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("app:test-password"))).body(Body::empty()).map_err(failure)?;
        assert_eq!(app.oneshot(request).await.map_err(failure)?.status(), StatusCode::CREATED);
        let started = matching_event(&mut subscription.events, |kind| matches!(kind, EventKind::PlaybackStarted { playback } if playback.id.as_str() == "bridge-play")).await?;
        assert!(matches!(started, EventKind::PlaybackStarted { playback } if playback.target_uri == "bridge:playback-mix"));
        receive_audio(&audio, -6000).await?;
        let finished = matching_event(&mut subscription.events, |kind| matches!(kind, EventKind::PlaybackFinished { playback } if playback.id.as_str() == "bridge-play")).await?;
        assert!(matches!(finished, EventKind::PlaybackFinished { playback } if playback.state == rusterisk_api::PlaybackState::Done));
        controller.execute(credentials()?, None, Action::Hangup(channel)).await?;
        finish_bye(&phone).await?;
        Ok(())
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    worker.close().await.map_err(failure)?;
    result
}

async fn finish_bye(socket: &UdpSocket) -> Result<(), ControlError> {
    let mut bytes = [0; 16385];
    for _ in 0..4 {
        let (size, peer) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
        else {
            return Err(ControlError::Invalid("Expected dialog request"));
        };
        if request.method == Method::Ack {
            continue;
        }
        assert_eq!(request.method, Method::Bye);
        let reply = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
        socket
            .send_to(reply.as_bytes(), peer)
            .await
            .map_err(failure)?;
        return Ok(());
    }
    Err(ControlError::Invalid("Expected dialog hangup"))
}

#[tokio::test]
async fn parallel_dial_bridges_received_audio_to_the_first_peer_and_cancels_the_other_invitation(
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
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[alice-contact]\ntype=aor\n[bob-contact]\ntype=aor\ncontact=sip:bob@{}\n[charlie-contact]\ntype=aor\ncontact=sip:charlie@{}\n[alice]\ntype=endpoint\ntransport=udp\nauth=auth\naors=alice-contact\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=auth\naors=bob-contact\nallow=ulaw\n[charlie]\ntype=endpoint\ntransport=udp\nauth=auth\naors=charlie-contact\nallow=ulaw\n", bob.local_addr().map_err(failure)?, charlie.local_addr().map_err(failure)?);
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Dial(PJSIP/bob&PJSIP/charlie,5)\nsame => n,Hangup()\n",
    )
    .map_err(failure)?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 4);
    let (media, hub, notices) = MediaHub::new(4);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let target = signaling.listeners()?[0].1;
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let result = async {
        invite(&alice, target, &alice_audio).await?;
        let mut bytes = [0; 16385];
        let (size, charlie_target) = tokio::time::timeout(Duration::from_secs(2), charlie.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
        let SipMessage::Request(invitation) = SipMessage::try_from(&bytes[..size]).map_err(failure)? else { return Err(ControlError::Invalid("Expected parallel invitation")); };
        assert_eq!(invitation.method, Method::Invite);
        let declined = |code: &str, cseq: &str| -> Result<String, ControlError> {
            Ok(format!("SIP/2.0 {code}\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=charlie\r\nCall-ID: {}\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n", invitation.top_via_header().map_err(failure)?.value(), invitation.from_header().map_err(failure)?.value(), invitation.to_header().map_err(failure)?.value(), invitation.call_id_header().map_err(failure)?.value()))
        };
        charlie.send_to(declined("180 Ringing", invitation.cseq_header().map_err(failure)?.value())?.as_bytes(), charlie_target).await.map_err(failure)?;
        assert_eq!(response(&alice).await?.status_code.code(), 180);
        let bob_target = answer(&bob, &bob_audio).await?;
        let answered = response(&alice).await?;
        assert_eq!(answered.status_code.code(), 200);
        let alice_target = Session::parse(std::str::from_utf8(&answered.body).map_err(failure)?, &[Codec::Pcmu]).map_err(failure)?.remote;
        let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-parallel-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.local_addr().map_err(failure)?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
        alice.send_to(ack.as_bytes(), target).await.map_err(failure)?;
        let (size, peer) = tokio::time::timeout(Duration::from_secs(2), charlie.recv_from(&mut bytes)).await.map_err(failure)?.map_err(failure)?;
        let SipMessage::Request(cancel) = SipMessage::try_from(&bytes[..size]).map_err(failure)? else { return Err(ControlError::Invalid("Expected cancelled parallel invitation")); };
        assert_eq!(cancel.method, Method::Cancel);
        charlie.send_to(declined("200 OK", cancel.cseq_header().map_err(failure)?.value())?.as_bytes(), peer).await.map_err(failure)?;
        charlie.send_to(declined("487 Request Terminated", invitation.cseq_header().map_err(failure)?.value())?.as_bytes(), peer).await.map_err(failure)?;
        let mut left = AudioPeer { socket: &alice_audio, target: alice_target, sender: RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?, sample: 4000 };
        let mut right = AudioPeer { socket: &bob_audio, target: bob_target, sender: RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?, sample: -7000 };
        exchange(&mut left, &mut right, (-7000, 4000)).await?;
        let channels = controller.execute(credentials()?, None, Action::ListChannels).await?.body.ok_or(ControlError::Capacity)?;
        let parent = channels.as_array().ok_or(ControlError::Invalid("Expected channel list"))?.iter().find(|channel| channel["name"].as_str().is_some_and(|name| name.starts_with("PJSIP/alice-"))).and_then(|channel| channel["id"].as_str()).ok_or(ControlError::NotFound("Expected caller channel"))?;
        controller.execute(credentials()?, None, Action::Hangup(id(parent)?)).await?;
        tokio::try_join!(finish_bye(&alice), finish_bye(&bob))?;
        assert_eq!(controller.execute(credentials()?, None, Action::ListChannels).await?.body, Some(serde_json::json!([])));
        Ok(())
    }.await;
    controller.shutdown();
    signaling.await.map_err(failure)??;
    hub.await.map_err(failure)?.map_err(failure)?;
    result
}
