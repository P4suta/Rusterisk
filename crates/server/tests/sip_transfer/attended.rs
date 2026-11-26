use super::{
    answer_dialog_at, exchange, failure, final_response, start_invitation,
    transfer_lifetime::{combine, finish},
    transfer_peer::Peer,
    transfer_signals::{acknowledge, RequestRoute, TransferSignals},
    AudioPeer, BranchNamespace, Invitation,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, Response, SipMessage};
use rusterisk_config::{Codec, Config};
use rusterisk_media::{AudioFormat, RtpSender};
use rusterisk_server::{media::MediaHub, signaling::Signaling, ControlError, Controller};
use rusterisk_sip::Session;
use rusterisk_test_support::Wait;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{net::UdpSocket, sync::mpsc};
#[path = "attended_reference.rs"]
mod reference;
#[path = "survivor_shutdown.rs"]
mod survivor_shutdown;

struct Dialog {
    answer: Response,
    callee: Request,
    callee_peer: SocketAddr,
    caller_media: SocketAddr,
    callee_media: SocketAddr,
}

async fn connect(
    caller: &Peer,
    callee: &Peer,
    target: SocketAddr,
    invitation: Invitation<'_>,
    media_ports: &mpsc::Sender<SocketAddr>,
) -> Result<Dialog, ControlError> {
    let dialed = invitation.extension;
    start_invitation(
        &caller.signal,
        target,
        caller.contact()?,
        caller.media()?,
        invitation,
    )
    .await?;
    let (callee_media, callee_dialog, expected_ack) = answer_dialog_at(
        &callee.signal,
        callee.contact()?,
        callee.media()?,
        Some(dialed),
    )
    .await?;
    media_ports.try_send(callee_media).map_err(failure)?;
    let answer = final_response(&caller.signal).await?;
    if answer.status_code.code() != 200 {
        return Err(failure(format!(
            "Expected answered consultation, received {}",
            answer.status_code.code()
        )));
    }
    let caller_media = Session::parse(
        std::str::from_utf8(&answer.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?
    .remote;
    media_ports.try_send(caller_media).map_err(failure)?;
    let remote = answer
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let branch = BranchNamespace::random()?.branch(0);
    let ack = format!("ACK {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", caller.contact()?, answer.from_header().map_err(failure)?.value(), answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
    caller
        .signal
        .send_to(ack.as_bytes(), target)
        .await
        .map_err(failure)?;
    acknowledge(&callee.signal, &expected_ack).await?;
    let callee = match SipMessage::try_from(callee_dialog.as_str()).map_err(failure)? {
        SipMessage::Request(request) => request,
        SipMessage::Response(_) => return Err(ControlError::Invalid("Expected callee dialog")),
    };
    Ok(Dialog {
        answer,
        callee,
        callee_peer: expected_ack.peer(),
        caller_media,
        callee_media,
    })
}

#[derive(Clone, Copy)]
enum Exercise {
    Consultation,
    Transfer,
    RetireSource,
    PeerEnded(Survivor),
}

#[derive(Clone, Copy)]
enum Survivor {
    Bob,
    Charlie,
}

enum CallListeners {
    Shared,
    Separate,
}

async fn exercise(
    peers: [Peer; 4],
    targets: [SocketAddr; 2],
    media_ports: mpsc::Sender<SocketAddr>,
    operation: Exercise,
    requests: [RequestRoute; 2],
) -> Result<(), ControlError> {
    let [first, bob, second, charlie] = peers;
    let [target, consultation_target] = targets;
    let initial = connect(
        &first,
        &bob,
        target,
        Invitation {
            extension: "100",
            call_id: "attended-source",
            from_tag: "source-caller",
        },
        &media_ports,
    )
    .await
    .map_err(|error| failure(format!("source dialog: {error}")))?;
    let mut first_voice = AudioPeer {
        socket: &first.audio,
        target: initial.caller_media,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [11; 10]).map_err(failure)?,
        sample: 4000,
    };
    let mut bob_voice = AudioPeer {
        socket: &bob.audio,
        target: initial.callee_media,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [12; 10]).map_err(failure)?,
        sample: -7000,
    };
    let consultation = connect(
        &second,
        &charlie,
        consultation_target,
        Invitation {
            extension: "200",
            call_id: "attended-consultation",
            from_tag: "consultation-caller",
        },
        &media_ports,
    )
    .await
    .map_err(|error| failure(format!("consultation dialog: {error}")))?;
    let mut second_voice = AudioPeer {
        socket: &second.audio,
        target: consultation.caller_media,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [13; 10]).map_err(failure)?,
        sample: 5000,
    };
    let mut charlie_voice = AudioPeer {
        socket: &charlie.audio,
        target: consultation.callee_media,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [14; 10]).map_err(failure)?,
        sample: 9000,
    };
    let source_audio = async {
        exchange(&mut first_voice, &mut bob_voice, (-7000, 4000))
            .await
            .map_err(|error| failure(format!("source received audio: {error}")))
    };
    let consultation_audio = async {
        exchange(&mut second_voice, &mut charlie_voice, (9000, 5000))
            .await
            .map_err(|error| failure(format!("consultation received audio: {error}")))
    };
    tokio::try_join!(source_audio, consultation_audio)?;
    if matches!(operation, Exercise::Consultation) {
        return Ok(());
    }
    let remote = initial
        .answer
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let referred = consultation
        .answer
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let local = consultation
        .answer
        .to_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    let remote_tag = consultation
        .answer
        .from_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?;
    let replaces = format!(
        "{}%3Bto-tag%3D{}%3Bfrom-tag%3D{}",
        consultation
            .answer
            .call_id_header()
            .map_err(failure)?
            .value(),
        local
            .tag()
            .ok_or(ControlError::Invalid("Missing consultation local tag"))?
            .value(),
        remote_tag
            .tag()
            .ok_or(ControlError::Invalid("Missing consultation remote tag"))?
            .value(),
    );
    let branch = BranchNamespace::random()?.branch(0);
    let refer = format!("REFER {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContact: <sip:phone@{}>\r\nRefer-To: <{referred}?Replaces={replaces}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", first.contact()?, initial.answer.from_header().map_err(failure)?.value(), initial.answer.to_header().map_err(failure)?.value(), initial.answer.call_id_header().map_err(failure)?.value(), first.contact()?);
    first
        .signal
        .send_to(refer.as_bytes(), target)
        .await
        .map_err(failure)?;
    let [source_requests, consultation_requests] = requests;
    let mut source = TransferSignals::new(&initial.answer, source_requests)?;
    source
        .accepted(&first.signal, target)
        .await
        .map_err(|error| failure(format!("attended REFER acceptance: {error}")))?;
    let consultation_peer = consultation.callee_peer;
    let consultation_dialog = consultation.callee;
    let mut consultation = TransferSignals::new(&consultation.answer, consultation_requests)?;
    let source_ended = async {
        let result = match operation {
            Exercise::RetireSource => {
                source
                    .retire_source(&first.signal, target, first.contact()?)
                    .await
            }
            Exercise::Consultation | Exercise::Transfer | Exercise::PeerEnded(_) => {
                source.settled(&first.signal, target).await
            }
        };
        result.map_err(|error| failure(format!("attended source termination: {error}")))
    };
    let consultation_ended = async {
        consultation
            .consultation_ended(&second.signal, consultation_target)
            .await
            .map_err(|error| failure(format!("consultation termination: {error}")))
    };
    let mut handoff_audio_verified = false;
    let received_audio = async {
        exchange(&mut bob_voice, &mut charlie_voice, (9000, -7000))
            .await
            .map_err(|error| failure(format!("attended received audio: {error}")))?;
        handoff_audio_verified = true;
        Ok::<(), ControlError>(())
    };
    if let Err(error) = tokio::try_join!(source_ended, consultation_ended, received_audio) {
        return Err(failure(format!(
            "{error}; source={:?}; consultation={:?}; handoff_audio_verified={handoff_audio_verified}",
            source.observed(),
            consultation.observed()
        )));
    }
    if matches!(operation, Exercise::RetireSource) {
        bob_voice.sample = -5000;
        charlie_voice.sample = 10000;
        exchange(&mut bob_voice, &mut charlie_voice, (10000, -5000))
            .await
            .map_err(|error| {
                failure(format!(
                    "post-retirement received audio: {error}; source={:?}; consultation={:?}",
                    source.observed(),
                    consultation.observed()
                ))
            })?;
    }
    println!("Observed attended transfer event={:?}", source.event()?);
    match operation {
        Exercise::Consultation | Exercise::Transfer => Ok(()),
        Exercise::RetireSource | Exercise::PeerEnded(Survivor::Bob) => {
            survivor_shutdown::end(
                &bob,
                &initial.callee,
                &charlie,
                &consultation_dialog,
                [target, consultation_peer],
            )
            .await
        }
        Exercise::PeerEnded(Survivor::Charlie) => {
            survivor_shutdown::end(
                &charlie,
                &consultation_dialog,
                &bob,
                &initial.callee,
                [consultation_target, initial.callee_peer],
            )
            .await
        }
    }
}

async fn native(operation: Exercise) -> Result<(), ControlError> {
    native_on(operation, CallListeners::Shared).await
}

async fn native_on(operation: Exercise, layout: CallListeners) -> Result<(), ControlError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let first = Peer::bind(ip, ip).await?;
    let bob = Peer::bind(ip, ip).await?;
    let second = Peer::bind(ip, ip).await?;
    let charlie = Peer::bind(ip, ip).await?;
    let sip = format!("[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n[alice-auth]\ntype=auth\nusername=phone\npassword=test-password\n[bob-auth]\ntype=auth\nusername=bob\npassword=bob-password\n[charlie-auth]\ntype=auth\nusername=charlie\npassword=charlie-password\n[alice-aor]\ntype=aor\n[bob-aor]\ntype=aor\ncontact=sip:bob@{}\n[charlie-aor]\ntype=aor\ncontact=sip:charlie@{}\n[alice]\ntype=endpoint\ntransport=udp\nauth=alice-auth\naors=alice-aor\ncontext=internal\nallow=ulaw\n[bob]\ntype=endpoint\ntransport=udp\nauth=bob-auth\naors=bob-aor\ncontext=internal\nallow=ulaw\n[charlie]\ntype=endpoint\ntransport=udp\nauth=charlie-auth\naors=charlie-aor\ncontext=internal\nallow=ulaw\n", bob.contact()?, charlie.contact()?);
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &sip,
        "",
        "[internal]\nexten => 100,1,Dial(PJSIP/${EXTEN}@bob,5)\nsame => n,Hangup()\nexten => 200,1,Dial(PJSIP/${EXTEN}@charlie,5)\nsame => n,Hangup()\n",
    ).map_err(failure)?;
    if matches!(layout, CallListeners::Separate) {
        let mut transport = config
            .transports
            .iter()
            .find(|transport| transport.name == "udp")
            .ok_or(ControlError::Invalid("Missing source transport"))?
            .clone();
        transport.name = "consultation-udp".into();
        config.transports.push(transport);
        let mut endpoint = config
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name == "alice")
            .ok_or(ControlError::Invalid("Missing source endpoint"))?
            .clone();
        endpoint.name = "consultation".into();
        endpoint.transport = "consultation-udp".into();
        config.endpoints.push(endpoint);
        config
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.name == "charlie")
            .ok_or(ControlError::Invalid("Missing consultation target"))?
            .transport = "consultation-udp".into();
    }
    config.rtp.start = 0;
    config.rtp.end = 0;
    let (controller, runner, _effects) = Controller::new(config.clone(), 8);
    let (media, hub, notices) = MediaHub::new(8);
    let signaling = Signaling::bind(config, controller.clone(), runner, media, notices).await?;
    let listeners = signaling.listeners()?;
    let target = listeners
        .iter()
        .find(|(name, _)| name == "udp")
        .ok_or(ControlError::Invalid("Missing source listener"))?
        .1;
    let consultation = match layout {
        CallListeners::Shared => target,
        CallListeners::Separate => {
            let consultation = listeners
                .iter()
                .find(|(name, _)| name == "consultation-udp")
                .ok_or(ControlError::Invalid("Missing consultation listener"))?
                .1;
            assert_ne!(consultation, target);
            consultation
        }
    };
    let mut addresses = Vec::with_capacity(20 + listeners.len());
    addresses.extend(listeners.into_iter().map(|(_, address)| address));
    for peer in [&first, &bob, &second, &charlie] {
        addresses.extend(peer.addresses()?);
    }
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let signaling = tokio::spawn(signaling.run(controller.shutdown_signal()));
    let (media_ports, mut server_ports) = mpsc::channel(4);
    let worker = tokio::spawn(exercise(
        [first, bob, second, charlie],
        [target, consultation],
        media_ports,
        operation,
        [
            RequestRoute::Direct(target),
            RequestRoute::Direct(consultation),
        ],
    ));
    let mut result = finish(worker).await;
    if result.is_ok() && matches!(operation, Exercise::RetireSource | Exercise::PeerEnded(_)) {
        let idle = Wait::Ready
            .bounded(async {
                let mut tick = tokio::time::interval(Duration::from_millis(20));
                loop {
                    tick.tick().await;
                    let channels = controller
                        .execute(
                            super::credentials()?,
                            None,
                            rusterisk_server::Action::ListChannels,
                        )
                        .await?;
                    if channels.body == Some(serde_json::json!([])) {
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| ControlError::Invalid("Attended transfer left a live channel"));
        result = idle.and_then(|observation| observation);
    }
    controller.shutdown();
    let (signaling, hub) = tokio::join!(finish(signaling), finish(hub),);
    for cleanup in [signaling, hub] {
        result = combine(result, cleanup);
    }
    for _ in 0..4 {
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

#[tokio::test]
async fn source_and_consultation_dialogs_exchange_known_audio_and_release_ports(
) -> Result<(), ControlError> {
    native(Exercise::Consultation).await
}

#[tokio::test]
async fn attended_refer_reuses_consultation_audio_and_retires_both_source_dialogs(
) -> Result<(), ControlError> {
    native(Exercise::Transfer).await
}

#[tokio::test]
async fn completed_attended_source_retirement_preserves_both_surviving_audio_streams(
) -> Result<(), ControlError> {
    native(Exercise::RetireSource).await
}

#[tokio::test]
async fn ending_either_surviving_dialog_releases_its_peer_after_attended_audio(
) -> Result<(), ControlError> {
    for peer in [Survivor::Bob, Survivor::Charlie] {
        native(Exercise::PeerEnded(peer)).await?;
    }
    Ok(())
}

#[tokio::test]
async fn attended_consultation_on_another_listener_preserves_known_audio_and_source_routes(
) -> Result<(), ControlError> {
    native_on(Exercise::Transfer, CallListeners::Separate).await
}

#[tokio::test]
#[ignore = "Requires the isolated Asterisk 22.9.0 reference."]
async fn reference_attended_transfer_matches_native_received_audio() -> Result<(), ControlError> {
    println!("Comparing attended transfer against the native server");
    native(Exercise::RetireSource)
        .await
        .map_err(|error| failure(format!("native attended transfer: {error}")))?;
    println!("Comparing attended transfer against Asterisk 22.9.0");
    reference::compare()
        .await
        .map_err(|error| failure(format!("Asterisk attended transfer: {error}")))
}
