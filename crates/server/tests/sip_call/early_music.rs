use super::{
    credentials, drain_received_audio, failure, matching_event, response, send_voice, stable_audio,
    start_invitation,
    transfer_lifetime::{combine, finish},
    transfer_peer::Peer,
    transfer_transactions::BranchNamespace,
    Invitation,
};
use reqwest::{Client, Method as HttpMethod, StatusCode};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Response, SipMessage};
use rusterisk_api::{Channel, ChannelId, ChannelState, EventKind};
use rusterisk_config::{Codec, Config};
use rusterisk_media::{AudioFormat, Frame, RtpPacket, RtpSender};
use rusterisk_server::{rtp::SocketPair, runtime::Server, ControlError, Controller};
use rusterisk_sip::Session;
use rusterisk_storage::Recorder;
use rusterisk_test_support::Wait;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};
use tokio::net::UdpSocket;

#[derive(Clone, Copy)]
enum Ending {
    Answer,
    Cancel,
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

fn configuration(root: &Path, media: &SocketPair) -> Result<Config, ControlError> {
    for (name, source) in [
        ("http.conf", "[general]\nbindaddr=127.0.0.1\n"),
        ("ari.conf", "[app]\ntype=user\npassword=test-password\n"),
        (
            "pjsip.conf",
            "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=phone\ncontext=internal\nallow=ulaw\n[phone]\ntype=aor\nmax_contacts=1\n",
        ),
        ("rtp.conf", ""),
        (
            "extensions.conf",
            "[internal]\nexten => 100,1,Stasis(app)\n",
        ),
        (
            "musiconhold.conf",
            "[default]\nmode=files\ndirectory=moh\nsort=alpha\n",
        ),
    ] {
        std::fs::write(root.join(name), source).map_err(failure)?;
    }
    let mut config = Config::load(root).map_err(failure)?;
    config.http.bind.set_port(0);
    config.rtp.start = media.local_addr().map_err(failure)?.port();
    config.rtp.end = media.control_addr().map_err(failure)?.port();
    Ok(config)
}

fn music(root: &Path) -> Result<(), ControlError> {
    let mut writer = Recorder::create(root, "early-known", 8000).map_err(failure)?;
    for _ in 0..4 {
        writer
            .write(Frame::new(&[4000; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
    }
    writer.finish().map_err(failure)?;
    let directory = root.join("moh");
    std::fs::create_dir(&directory).map_err(failure)?;
    std::fs::rename(
        root.join("recordings/early-known.wav"),
        directory.join("known.wav"),
    )
    .map_err(failure)
}

async fn request(
    client: &Client,
    address: SocketAddr,
    method: HttpMethod,
    path: &str,
) -> Result<reqwest::Response, ControlError> {
    client
        .request(method, format!("http://{address}/ari/{path}"))
        .basic_auth("app", Some("test-password"))
        .send()
        .await
        .map_err(failure)
}

async fn command(
    client: &Client,
    address: SocketAddr,
    method: HttpMethod,
    path: &str,
) -> Result<(), ControlError> {
    let response = request(client, address, method, path).await?;
    if response.status() != StatusCode::NO_CONTENT {
        return Err(failure(format!(
            "Expected HTTP 204, received {}",
            response.status().as_u16()
        )));
    }
    Ok(())
}

async fn state(
    client: &Client,
    address: SocketAddr,
    channel: &ChannelId,
    expected: ChannelState,
) -> Result<(), ControlError> {
    let response = request(
        client,
        address,
        HttpMethod::GET,
        &format!("channels/{channel}"),
    )
    .await?;
    if response.status() != StatusCode::OK {
        return Err(ControlError::Invalid("Expected a live early channel"));
    }
    let snapshot: Channel = response.json().await.map_err(failure)?;
    require(
        &snapshot.id == channel && snapshot.state == expected,
        "Unexpected channel identity or state",
    )
}

async fn connection(
    controller: &Controller,
    channel: &ChannelId,
    expected: &str,
) -> Result<(), ControlError> {
    let result = controller
        .execute(
            credentials()?,
            None,
            rusterisk_server::Action::GetVariable {
                channel: channel.clone(),
                variable: "RUSTERISK_MEDIA_CONNECTED".into(),
            },
        )
        .await?;
    require(
        result.body == Some(serde_json::json!({"value": expected})),
        "Unexpected media connection milestone",
    )
}

async fn signaling_response(phone: &Peer, target: SocketAddr) -> Result<Response, ControlError> {
    let mut bytes = [0; 16385];
    let (length, source) = Wait::Ready
        .bounded(phone.signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    require(source == target, "Unexpected signaling peer")?;
    let SipMessage::Response(response) = SipMessage::try_from(&bytes[..length]).map_err(failure)?
    else {
        return Err(ControlError::Invalid(
            "Expected an early signaling response",
        ));
    };
    Ok(response)
}

fn same_invitation(reply: &Response, original: &Response) -> Result<(), ControlError> {
    let sequence = reply
        .cseq_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    require(
        reply.top_via_header().map_err(failure)? == original.top_via_header().map_err(failure)?
            && reply.call_id_header().map_err(failure)?
                == original.call_id_header().map_err(failure)?
            && reply.from_header().map_err(failure)? == original.from_header().map_err(failure)?
            && sequence.seq == 2
            && sequence.method == Method::Invite,
        "Response did not match the authenticated INVITE transaction",
    )
}

fn audio_packet(
    bytes: &[u8],
    source: SocketAddr,
    expected: SocketAddr,
    sample: i32,
) -> Result<u32, ControlError> {
    require(source == expected, "Generated audio changed its RTP peer")?;
    let packet = RtpPacket::parse(bytes).map_err(failure)?;
    require(
        packet.payload_type() == 0
            && packet.payload().len() == 160
            && packet.payload().iter().all(|encoded| {
                (i32::from(rusterisk_media::mulaw_decode(*encoded)) - sample).abs() < 300
            }),
        "Received PCMU did not match the expected generated audio permission",
    )?;
    Ok(packet.ssrc())
}

async fn initially_muted_transport(
    phone: &Peer,
    expected: SocketAddr,
) -> Result<u32, ControlError> {
    Wait::Ready
        .bounded(async {
            let mut bytes = [0; 1501];
            let mut ssrc = None;
            for _ in 0..6 {
                let (length, source) = phone
                    .audio
                    .data()
                    .recv_from(&mut bytes)
                    .await
                    .map_err(failure)?;
                let received = audio_packet(&bytes[..length], source, expected, 0)?;
                require(
                    ssrc.is_none_or(|previous| previous == received),
                    "Initial muted media changed its RTP sender",
                )?;
                ssrc = Some(received);
            }
            ssrc.ok_or(ControlError::Invalid("Expected received muted RTP"))
        })
        .await
        .map_err(failure)?
}

async fn transport(phone: &Peer, expected: SocketAddr, sample: i32) -> Result<u32, ControlError> {
    drain_received_audio(&phone.audio)?;
    stable_audio(&phone.audio, sample).await?;
    let mut bytes = [0; 1501];
    let (length, source) = Wait::Ready
        .bounded(phone.audio.data().recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    audio_packet(&bytes[..length], source, expected, sample)
}

async fn released_pair(address: SocketAddr) -> Result<SocketPair, ControlError> {
    let control = address
        .port()
        .checked_add(1)
        .ok_or(ControlError::Capacity)?;
    Wait::Ready
        .bounded(async {
            let mut poll = tokio::time::interval(Duration::from_millis(20));
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                poll.tick().await;
                match SocketPair::bind(address.ip(), address.port(), control) {
                    Ok(pair) => return Ok(pair),
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                    Err(error) => return Err(failure(error)),
                }
            }
        })
        .await
        .map_err(failure)?
}

async fn acknowledge(
    phone: &Peer,
    target: SocketAddr,
    answer: &Response,
) -> Result<(), ControlError> {
    let contact = rsipstack::sip::headers::typed::Contact::parse(
        answer.contact_header().map_err(failure)?.value(),
    )
    .map_err(failure)?;
    let branch = BranchNamespace::random()?.branch(0);
    let packet = format!("ACK {} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", contact.uri, phone.contact()?, answer.from_header().map_err(failure)?.value(), answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
    phone
        .signal
        .send_to(packet.as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

async fn receive_hangup(
    phone: &Peer,
    target: SocketAddr,
    answer: &Response,
) -> Result<(), ControlError> {
    let mut bytes = [0; 16385];
    let (length, source) = Wait::Ready
        .bounded(phone.signal.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    require(source == target, "Unexpected BYE peer")?;
    let SipMessage::Request(bye) = SipMessage::try_from(&bytes[..length]).map_err(failure)? else {
        return Err(ControlError::Invalid("Expected source termination"));
    };
    require(
        bye.method == Method::Bye
            && bye
                .cseq_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .method
                == Method::Bye
            && bye.call_id_header().map_err(failure)?
                == answer.call_id_header().map_err(failure)?
            && bye.from_header().map_err(failure)?.value()
                == answer.to_header().map_err(failure)?.value()
            && bye.to_header().map_err(failure)?.value()
                == answer.from_header().map_err(failure)?.value(),
        "BYE did not match the answered dialog",
    )?;
    let reply = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", bye.top_via_header().map_err(failure)?.value(), bye.from_header().map_err(failure)?.value(), bye.to_header().map_err(failure)?.value(), bye.call_id_header().map_err(failure)?.value(), bye.cseq_header().map_err(failure)?.value());
    phone
        .signal
        .send_to(reply.as_bytes(), target)
        .await
        .map_err(failure)?;
    Ok(())
}

async fn exercise(root: &Path, ending: Ending) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let phone = Peer::bind(ip, ip).await?;
    let reservation = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let local_media = reservation.local_addr().map_err(failure)?;
    let mut addresses = phone.addresses()?.to_vec();
    addresses.extend([local_media, reservation.control_addr().map_err(failure)?]);
    let config_root = root.join("configuration");
    let data = root.join("data");
    std::fs::create_dir(&config_root).map_err(failure)?;
    std::fs::create_dir(&data).map_err(failure)?;
    let config = configuration(&config_root, &reservation)?;
    music(&data)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(failure)?;
    let server = Server::bind(config, 4, data).await?;
    let controller = server.controller();
    let address = server.local_addr();
    let listeners = server.sip_listeners();
    let server = tokio::spawn(server.run());
    let mut reservation = Some(reservation);
    let mut channel = None;
    let mut phase = "listener addresses";
    let mut subscription = None;
    let result = Wait::Ready.bounded(async {
        let address = *address.as_ref().map_err(failure)?;
        let listeners = listeners.as_ref().map_err(failure)?;
        require(listeners.len() == 1, "Expected one SIP listener")?;
        let target = listeners[0].1;
        addresses.push(target);
        subscription = Some(controller.subscribe(credentials()?, None, "app".into()).await?);
        let events = &mut subscription.as_mut().ok_or(ControlError::Capacity)?.events;
        phase = "authenticated incoming invitation";
        start_invitation(&phone.signal, target, phone.contact()?, phone.media()?, Invitation {
            extension: "100", call_id: "early-music", from_tag: "early-caller",
        }).await?;
        let trying = response(&phone.signal).await?;
        require(trying.status_code.code() == 100, "Expected authenticated INVITE 100")?;
        phase = "ringing Stasis entry";
        let EventKind::StasisStart { channel: incoming, args } = matching_event(events, |event| matches!(event, EventKind::StasisStart { .. })).await? else {
            return Err(ControlError::Invalid("Expected ringing Stasis entry"));
        };
        require(incoming.state == ChannelState::Ring && args.is_empty(), "Expected ringing Stasis entry with no arguments")?;
        channel = Some(incoming.id.clone());
        let id = &incoming.id;
        command(&client, address, HttpMethod::POST, &format!("channels/{id}/ring")).await?;
        let ringing = signaling_response(&phone, target).await?;
        require(ringing.status_code.code() == 180, "Expected INVITE 180")?;
        same_invitation(&ringing, &trying)?;
        phase = "initial outgoing mute before early media";
        command(&client, address, HttpMethod::POST, &format!("channels/{id}/mute?direction=out")).await?;
        phase = "accepted early music";
        drop(reservation.take());
        command(&client, address, HttpMethod::POST, &format!("channels/{id}/moh")).await?;
        phase = "183 with prepared media";
        let progress = signaling_response(&phone, target).await?;
        require(progress.status_code.code() == 183, "Expected early INVITE 183")?;
        require(progress.headers.iter().any(|header| header.name().eq_ignore_ascii_case("Content-Type") && header.value() == "application/sdp"), "Early progress omitted its SDP content type")?;
        same_invitation(&progress, &trying)?;
        require(progress.to_header().map_err(failure)? == ringing.to_header().map_err(failure)?, "Early progress changed the dialog tag")?;
        let negotiated = Session::parse(std::str::from_utf8(&progress.body).map_err(failure)?, &[Codec::Pcmu]).map_err(failure)?;
        require(negotiated.remote == local_media, "Early SDP did not advertise the reserved RTP pair")?;
        connection(&controller, id, "0").await?;
        phase = "initial mute prevents generated audio from the first early RTP frame";
        let ssrc = initially_muted_transport(&phone, local_media).await?;
        phase = "unmute after 183 enables known audio while Ring";
        command(&client, address, HttpMethod::DELETE, &format!("channels/{id}/mute?direction=out")).await?;
        require(transport(&phone, local_media, 4000).await? == ssrc, "Early unmute replaced the RTP sender")?;
        phase = "mute after 183 stops known audio while Ring";
        command(&client, address, HttpMethod::POST, &format!("channels/{id}/mute?direction=out")).await?;
        require(transport(&phone, local_media, 0).await? == ssrc, "Early mute replaced the RTP sender")?;
        phase = "second unmute after 183 restores known audio while Ring";
        command(&client, address, HttpMethod::DELETE, &format!("channels/{id}/mute?direction=out")).await?;
        require(transport(&phone, local_media, 4000).await? == ssrc, "Early unmute replaced the RTP sender")?;
        state(&client, address, id, ChannelState::Ring).await?;
        connection(&controller, id, "0").await?;
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [83; 10]).map_err(failure)?;
        send_voice(&phone.audio, local_media, &mut sender, -7000).await?;
        matching_event(events, |event| matches!(event, EventKind::ChannelVarset { channel, variable, value } if &channel.id == id && variable == "RUSTERISK_MEDIA_CONNECTED" && value == "1")).await?;
        connection(&controller, id, "1").await?;
        match ending {
            Ending::Answer => {
                phase = "HTTP Answer reuses early transport";
                command(&client, address, HttpMethod::POST, &format!("channels/{id}/answer")).await?;
                let answer = signaling_response(&phone, target).await?;
                require(answer.status_code.code() == 200, "Expected INVITE 200")?;
                same_invitation(&answer, &trying)?;
                require(answer.body == progress.body
                    && answer.to_header().map_err(failure)? == progress.to_header().map_err(failure)?
                    && answer.contact_header().map_err(failure)? == progress.contact_header().map_err(failure)?,
                    "Answer changed the published early SDP or dialog route")?;
                acknowledge(&phone, target, &answer).await?;
                matching_event(events, |event| matches!(event, EventKind::ChannelStateChange { channel } if &channel.id == id && channel.state == ChannelState::Up)).await?;
                phase = "new known received audio after Answer";
                require(transport(&phone, local_media, 4000).await? == ssrc, "Answer replaced the early RTP sender")?;
                state(&client, address, id, ChannelState::Up).await?;
                phase = "stop music and terminate answered dialog";
                command(&client, address, HttpMethod::DELETE, &format!("channels/{id}/moh")).await?;
                drain_received_audio(&phone.audio)?;
                stable_audio(&phone.audio, 0).await?;
                command(&client, address, HttpMethod::DELETE, &format!("channels/{id}")).await?;
                receive_hangup(&phone, target, &answer).await?;
            }
            Ending::Cancel => {
                phase = "CANCEL terminates early invitation";
                let packet = format!("CANCEL sip:100@{target} SIP/2.0\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 CANCEL\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", trying.top_via_header().map_err(failure)?.value(), trying.from_header().map_err(failure)?.value(), trying.to_header().map_err(failure)?.value(), trying.call_id_header().map_err(failure)?.value());
                phone.signal.send_to(packet.as_bytes(), target).await.map_err(failure)?;
                let mut cancel_seen = false;
                let mut terminated = None;
                for _ in 0..2 {
                    let reply = signaling_response(&phone, target).await?;
                    require(reply.call_id_header().map_err(failure)? == progress.call_id_header().map_err(failure)?
                        && reply.top_via_header().map_err(failure)? == trying.top_via_header().map_err(failure)?
                        && reply.from_header().map_err(failure)? == trying.from_header().map_err(failure)?
                        && reply.to_header().map_err(failure)? == progress.to_header().map_err(failure)?,
                        "CANCEL response changed the early dialog identity")?;
                    let sequence = reply.cseq_header().map_err(failure)?.typed().map_err(failure)?;
                    require(sequence.seq == 2, "Unexpected CANCEL transaction sequence")?;
                    match (reply.status_code.code(), sequence.method) {
                        (200, Method::Cancel) => { require(!cancel_seen, "Repeated CANCEL reply before INVITE termination")?; cancel_seen = true; }
                        (487, Method::Invite) => { require(terminated.is_none(), "Repeated INVITE termination before CANCEL reply")?; terminated = Some(reply); }
                        _ => return Err(ControlError::Invalid("Unexpected early cancellation response")),
                    }
                }
                require(cancel_seen, "Missing CANCEL 200")?;
                let terminated = terminated.ok_or(ControlError::Invalid("Missing INVITE 487"))?;
                let ack = format!("ACK sip:100@{target} SIP/2.0\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", trying.top_via_header().map_err(failure)?.value(), terminated.from_header().map_err(failure)?.value(), terminated.to_header().map_err(failure)?.value(), terminated.call_id_header().map_err(failure)?.value());
                phone.signal.send_to(ack.as_bytes(), target).await.map_err(failure)?;
            }
        }
        phase = "terminal channel publication";
        matching_event(events, |event| matches!(event, EventKind::ChannelDestroyed { channel, .. } if &channel.id == id)).await?;
        let late = request(&client, address, HttpMethod::POST, &format!("channels/{id}/answer")).await?;
        require(late.status() == StatusCode::NOT_FOUND, "A terminated early channel accepted a late Answer")?;
        if matches!(ending, Ending::Cancel) {
            phase = "CANCEL releases RTP while Server remains running";
            drop(released_pair(local_media).await?);
            phase = "Server remains responsive after early cancellation";
            let live = request(&client, address, HttpMethod::GET, "channels").await?;
            require(live.status() == StatusCode::OK, "Server stopped before early RTP release was observed")?;
            let channels: Vec<Channel> = live.json().await.map_err(failure)?;
            require(channels.is_empty(), "Early cancellation left a live channel")?;
        }
        Ok(())
    }).await.map_err(failure).and_then(|result| result);
    let mut result = result.map_err(|error| failure(format!("early music phase={phase}: {error}")));
    if let (Ok(address), Some(channel)) = (&address, &channel) {
        let cleanup = request(
            &client,
            *address,
            HttpMethod::DELETE,
            &format!("channels/{channel}"),
        )
        .await
        .and_then(|reply| {
            if matches!(
                reply.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
            ) {
                Ok(())
            } else {
                Err(failure(format!(
                    "Channel cleanup HTTP {}",
                    reply.status().as_u16()
                )))
            }
        });
        result = combine(result, cleanup);
    }
    controller.shutdown();
    result = combine(result, finish(server).await);
    drop(subscription);
    drop(reservation);
    drop(phone);
    let mut rebound = Vec::with_capacity(addresses.len());
    for address in addresses {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => {
                result = combine(
                    result,
                    Err(failure(format!("UDP rebind {address}: {error}"))),
                )
            }
        }
    }
    if let Ok(address) = address {
        result = combine(
            result,
            tokio::net::TcpListener::bind(address)
                .await
                .map(drop)
                .map_err(failure),
        );
    }
    result
}

async fn run(ending: Ending) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = exercise(directory.path(), ending).await;
    combine(result, directory.close().map_err(failure))
}

#[tokio::test]
async fn early_music_is_received_while_ringing_and_answer_reuses_the_same_transport(
) -> Result<(), ControlError> {
    run(Ending::Answer).await
}

#[tokio::test]
async fn cancelled_early_music_cannot_be_answered_and_releases_owned_ports(
) -> Result<(), ControlError> {
    run(Ending::Cancel).await
}
