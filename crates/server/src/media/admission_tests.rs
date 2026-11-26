use super::*;
use rusterisk_api::ChannelId;
use rusterisk_media::AudioFormat;
use rusterisk_test_support::Wait;
use std::net::Ipv4Addr;
use tokio::net::{TcpListener, TcpStream};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key() -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from("existing".to_owned()).map_err(failure)?,
        generation: 19,
    })
}

#[tokio::test]
async fn both_phones_receive_audio_while_ordinary_notices_are_full() -> Result<(), MediaError> {
    let (_port, mut hub, mut notices) = MediaHub::new(2);
    let left = named("left")?;
    let right = CallKey {
        id: ChannelId::try_from("right".to_owned()).map_err(failure)?,
        generation: 2,
    };
    let first = owned_leg(&mut hub, &left)?;
    let second = owned_leg(&mut hub, &right)?;
    hub.mixer.bridge(1, &[left.clone(), right.clone()])?;
    for _ in 0..hub.notices.ordinary.max_capacity() {
        hub.notice(MediaNotice::Answer { key: left.clone() })?;
    }
    for (peer, key, sample) in [(&first, &left, 0x55), (&second, &right, 0x22)] {
        let leg = hub.legs.get(key).ok_or(MediaError::UnknownStream)?;
        let packet = rusterisk_media::RtpPacket::new(0, 1, 160, 7, &[sample; 160])?;
        peer.data()
            .send_to(&packet.encode()?, leg.local_addr()?)
            .await
            .map_err(failure)?;
        Wait::Ready
            .bounded(leg.input_observer()?.peek_from(&mut [0; 1]))
            .await
            .map_err(failure)?
            .map_err(failure)?;
    }
    hub.tick()?;
    for (peer, expected) in [(&first, 0x22), (&second, 0x55)] {
        let mut bytes = [0; 1501];
        let (size, _) = Wait::Ready
            .bounded(peer.data().recv_from(&mut bytes))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        let packet = rusterisk_media::RtpPacket::parse(&bytes[..size])?;
        assert_eq!(packet.payload(), &[expected; 160]);
    }
    let mut connected = std::collections::BTreeSet::new();
    while let Ok(notice) = notices.try_recv() {
        if let MediaNotice::Connected { key } = notice {
            assert!(connected.insert(key));
        }
    }
    assert_eq!(connected, [left, right].into());
    hub.close().await
}

#[tokio::test]
async fn measurement_reply_preserves_the_capture_time_and_counts() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let mut leg = crate::rtp::RtpLeg::new(
        sockets,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: rusterisk_media::AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        PeerMediaDirection::SendReceive,
    )?;
    leg.measurements_mut().local.rxcount = 7;
    let port = Port::owned(
        PeerMediaDirection::SendReceive,
        DigitSupport::Unsupported,
        hub.notices.reserve_leg()?,
    );
    hub.install_link(key.clone(), port, Gate::Open, 8000)?;
    hub.legs.insert(key.clone(), leg);
    let (reply, response) = oneshot::channel();
    let before = crate::utc_timestamps::now_millis();
    hub.command(Command::Measurements { key, reply })?;
    let after = crate::utc_timestamps::now_millis();
    if let Some(leg) = hub.legs.values_mut().next() {
        leg.measurements_mut().local.rxcount = 8;
    }
    hub.close().await?;
    let (captured, sample) = response
        .await
        .map_err(failure)?
        .ok_or(MediaError::UnknownStream)?;
    assert!((before..=after).contains(&captured));
    assert_eq!(sample.local.rxcount, 7);
    Ok(())
}

#[tokio::test]
async fn a_failed_leg_reports_its_stop_and_returns_its_ports_under_notice_pressure(
) -> Result<(), MediaError> {
    let (_handle, mut hub, mut notices) = MediaHub::new(1);
    let original = key()?;
    let peer = owned_leg(&mut hub, &original)?;
    let address = hub
        .legs
        .get(&original)
        .ok_or(MediaError::UnknownStream)?
        .local_addr()?;
    for _ in 0..hub.notices.ordinary.max_capacity() {
        hub.notice(MediaNotice::Answer {
            key: original.clone(),
        })?;
    }
    hub.failed
        .push((original.clone(), MediaError::InvalidPacket));
    hub.fail_legs()?;
    hub.release_call(original.clone())?;
    let mut stops = 0;
    while let Ok(notice) = notices.try_recv() {
        if let MediaNotice::Stopped { key, result } = notice {
            assert_eq!(key, original);
            assert!(matches!(result, Err(MediaError::InvalidPacket)));
            stops += 1;
        }
    }
    assert_eq!(stops, 1);
    let (_shutdown, mut stop) = watch::channel(false);
    hub.turn(&mut stop, &mut FrameClock::new()).await?;
    assert!(matches!(notices.try_recv(), Ok(MediaNotice::Released { key }) if key == original));
    let replacement = CallKey {
        id: original.id,
        generation: original.generation + 64,
    };
    hub.attach(
        replacement.clone(),
        SocketPair::bind(
            address.ip(),
            address.port(),
            address
                .port()
                .checked_add(1)
                .ok_or(MediaError::InvalidRoute)?,
        )
        .map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    speak(&peer, &hub, &replacement, 1).await?;
    received(&mut hub, &replacement, 1).await?;
    hub.close().await
}

fn datagram(hub: &mut MediaHub, key: CallKey, peer: &SocketPair) -> Result<(), MediaError> {
    hub.attach(
        key,
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )
}

async fn stream() -> Result<(TcpStream, TcpStream), MediaError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(failure)?;
    let (peer, (socket, _)) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().map_err(failure)?),
        listener.accept()
    )
    .map_err(failure)?;
    Ok((peer, socket))
}

async fn rejected(
    hub: &mut MediaHub,
    key: &CallKey,
    result: Result<(), MediaError>,
) -> Result<(), MediaError> {
    assert!(matches!(result, Err(MediaError::InvalidRoute)));
    let (reply, response) = oneshot::channel();
    drop(response);
    hub.complete_attachment(key, result, reply)?;
    let preserved = hub.ports.get(key).is_some_and(|port| match &port.io {
        super::Io::Task(io) => !*io.stop.borrow(),
        super::Io::Owned => hub.legs.contains_key(key),
    });
    hub.close().await?;
    assert!(
        preserved,
        "a rejected attachment cannot release an existing link when its reply receiver closes"
    );
    Ok(())
}

#[tokio::test]
async fn early_attachment_closes_conversation_and_generated_audio_before_publication(
) -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let early = key()?;
    let other = CallKey {
        id: ChannelId::try_from("peer".to_owned()).map_err(failure)?,
        generation: 20,
    };
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let result = (|| {
        hub.attach(
            early.clone(),
            SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
            RtpSession {
                remote: peer.local_addr().map_err(failure)?,
                format: AudioFormat::Pcmu,
                payload_type: 0,
                telephone: None,
            },
            PeerMediaDirection::SendReceive,
            AttachmentMode::Early,
        )?;
        datagram(&mut hub, other.clone(), &peer)?;
        hub.mixer.bridge(1, &[early.clone(), other.clone()])?;
        hub.mixer.push(&early, Frame::new(&[4000; 160], 8000)?)?;
        hub.mixer.push(&other, Frame::new(&[-7000; 160], 8000)?)?;
        let audio = hub.mixer.tick_with_playback(&[(
            AudioTarget::Channel(early.clone()),
            Frame::new(&[1234; 960], 48000)?,
        )])?;
        if !hub
            .ports
            .get(&early)
            .is_some_and(|port| port.generated_audio_gate == Gate::Closed)
            || audio.channels.len() != 2
            || audio
                .channels
                .iter()
                .any(|(_, frame)| frame.samples().iter().any(|sample| *sample != 0))
        {
            return Err(MediaError::InvalidRoute);
        }
        Ok(())
    })();
    let closed = hub.close().await;
    match (result, closed) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(MediaError::Transport(format!(
            "{primary}; early admission cleanup failed: {cleanup}"
        ))),
    }
}

#[tokio::test]
async fn cancelled_duplicate_datagram_admission_keeps_the_existing_link() -> Result<(), MediaError>
{
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    datagram(&mut hub, key.clone(), &peer)?;
    let result = datagram(&mut hub, key.clone(), &peer);
    rejected(&mut hub, &key, result).await
}

#[tokio::test]
async fn cancelled_duplicate_stream_admission_keeps_the_existing_link() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let (_original_peer, socket) = stream().await?;
    hub.attach_audiosocket(key.clone(), socket, 8000)?;
    let (_duplicate_peer, socket) = stream().await?;
    let result = hub.attach_audiosocket(key.clone(), socket, 8000);
    rejected(&mut hub, &key, result).await
}

#[tokio::test]
async fn cancelled_successful_admission_releases_the_new_link() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    datagram(&mut hub, key.clone(), &peer)?;
    let (reply, response) = oneshot::channel();
    drop(response);
    hub.complete_attachment(&key, Ok(()), reply)?;
    let removed = !hub.ports.contains_key(&key);
    hub.close().await?;
    assert!(removed);
    Ok(())
}

fn browser() -> Result<crate::webrtc::WebRtcSession, MediaError> {
    let offering = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let answering = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let now = std::time::Instant::now();
    let codecs = [rusterisk_config::Codec::Pcmu];
    let offer = crate::webrtc::WebRtcOffer::new(
        offering,
        answering.local_addr().map_err(failure)?,
        &codecs,
        now,
    )?;
    let local = answering.local_addr().map_err(failure)?;
    let (session, _answer) =
        crate::webrtc::WebRtcSession::answer(answering, local, offer.description(), &codecs, now)?;
    Ok(session)
}

// Runs all queued media commands in arrival order.
fn settle(hub: &mut MediaHub) -> Result<(), MediaError> {
    let command = hub.commands.try_recv().map_err(failure)?;
    hub.command(command)?;
    while let Ok(command) = hub.commands.try_recv() {
        hub.command(command)?;
    }
    Ok(())
}

#[tokio::test]
async fn a_bridge_command_finds_the_webrtc_attachment_that_was_queued_before_it(
) -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let _receipt = handle.queue_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    handle.bridge(7, Vec::from([key.clone()]))?;
    settle(&mut hub)?;
    let attached = hub.ports.contains_key(&key);
    let stopped = notices.try_recv().is_ok();
    hub.close().await?;
    assert!(attached);
    assert!(!stopped);
    Ok(())
}

#[tokio::test]
async fn an_attachment_without_a_reply_reports_its_result_as_a_notice() -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    // The media task refuses a second link for one call.
    for (serial, admitted) in [(31, true), (32, false)] {
        handle.start_attachment(
            key.clone(),
            SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
            RtpSession {
                remote: peer.local_addr().map_err(failure)?,
                format: AudioFormat::Pcmu,
                payload_type: 0,
                telephone: None,
            },
            PeerMediaDirection::SendReceive,
            AttachmentMode::Answered,
            serial,
        )?;
        settle(&mut hub)?;
        let reported = matches!(
            notices.try_recv(),
            Ok(MediaNotice::Attached { key: attached, serial: numbered, result })
                if attached == key && numbered == serial && result.is_ok() == admitted
        );
        assert!(reported, "admitted={admitted}");
    }
    handle.start_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
        33,
    )?;
    handle.release(key.clone())?;
    settle(&mut hub)?;
    let refused = matches!(
        notices.try_recv(),
        Ok(MediaNotice::Attached { key: attached, serial: 33, result: Err(_) }) if attached == key
    );
    hub.close().await?;
    assert!(refused);
    Ok(())
}

#[tokio::test]
async fn ordinary_notices_keep_their_capacity_while_a_webrtc_attachment_waits(
) -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    for _ in 0..127 {
        hub.notice(MediaNotice::Connected { key: key.clone() })?;
    }
    let _release = handle.start_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
        1,
    )?;
    hub.notice(MediaNotice::Connected { key: key.clone() })?;
    assert!(matches!(
        hub.notice(MediaNotice::Connected { key: key.clone() }),
        Err(MediaError::Capacity)
    ));
    let _second_release = handle.start_webrtc_attachment(
        CallKey {
            id: ChannelId::try_from("second".to_owned()).map_err(failure)?,
            generation: 1,
        },
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
        2,
    )?;
    settle(&mut hub)?;
    for _ in 0..128 {
        assert!(
            matches!(notices.try_recv(), Ok(MediaNotice::Connected { key: received }) if received == key)
        );
    }
    for serial in [1, 2] {
        assert!(
            matches!(notices.try_recv(), Ok(MediaNotice::Attached { serial: received, result: Ok(()), .. }) if received == serial)
        );
    }
    hub.close().await
}

#[tokio::test]
async fn accepted_direction_keeps_its_notice_slot_under_load() -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let direction = |serial| {
        handle.direction_order(
            key.clone(),
            PeerMediaDirection::SendOnly,
            Gate::Open,
            Gate::Open,
            serial,
        )
    };
    handle.send(direction(1)?)?;
    let mut pending = Vec::new();
    while let Ok(slot) = hub.notices.completions.clone().try_reserve_owned() {
        pending.push(slot);
    }
    assert_eq!(pending.len(), 127);
    for _ in 0..128 {
        hub.notice(MediaNotice::Connected { key: key.clone() })?;
    }
    let refused = direction(2);
    settle(&mut hub)?;
    let mut completed = Vec::new();
    while let Ok(notice) = notices.try_recv() {
        if let MediaNotice::DirectionApplied { serial, result, .. } = notice {
            completed.push((serial, result?));
        }
    }
    hub.close().await?;
    assert!(matches!(refused, Err(MediaError::Capacity)));
    assert_eq!(completed, [(1, false)]);
    Ok(())
}

#[tokio::test]
async fn a_direction_change_without_a_reply_reports_whether_it_applied() -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    for (serial, applied) in [(41, false), (42, true)] {
        let order = handle.direction_order(
            key.clone(),
            PeerMediaDirection::SendOnly,
            Gate::OutgoingMuted,
            Gate::OutgoingMuted,
            serial,
        )?;
        handle.send(order)?;
        settle(&mut hub)?;
        let reported = matches!(
            notices.try_recv(),
            Ok(MediaNotice::DirectionApplied {
                key: changed,
                serial: numbered,
                result: Ok(result),
            }) if changed == key && numbered == serial && result == applied
        );
        assert!(reported, "applied={applied}");
        if !applied {
            datagram(&mut hub, key.clone(), &peer)?;
        }
    }
    hub.close().await
}

#[tokio::test]
async fn a_replacement_webrtc_answer_survives_release_of_the_expired_attachment(
) -> Result<(), MediaError> {
    let (handle, mut hub, mut notices) = MediaHub::new(4);
    let key = key()?;
    let expired = handle.start_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Early,
        1,
    )?;
    expired.send(Order::Release(key.clone()));
    let _replacement = handle.start_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
        2,
    )?;
    settle(&mut hub)?;
    assert!(matches!(
        notices.try_recv(),
        Ok(MediaNotice::Attached {
            serial: 1,
            result: Ok(()),
            ..
        })
    ));
    assert!(
        matches!(notices.try_recv(), Ok(MediaNotice::Released { key: released }) if released == key)
    );
    assert!(matches!(
        notices.try_recv(),
        Ok(MediaNotice::Attached {
            serial: 2,
            result: Ok(()),
            ..
        })
    ));
    let installed = hub.ports.contains_key(&key) && hub.webrtc_legs.contains_key(&key);
    assert!(hub.workers.is_empty());
    hub.close().await?;
    assert!(installed, "the expired link cannot remove the replacement");
    Ok(())
}

#[tokio::test]
async fn a_release_ends_the_webrtc_attachment_that_was_queued_before_it() -> Result<(), MediaError>
{
    let (handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let _receipt = handle.queue_webrtc_attachment(
        key.clone(),
        browser()?,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    handle.release(key.clone())?;
    settle(&mut hub)?;
    let remaining = hub.ports.contains_key(&key);
    hub.close().await?;
    assert!(!remaining);
    Ok(())
}

fn named(name: &str) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(name.to_owned()).map_err(failure)?,
        generation: 1,
    })
}

// Attaches one protocol leg that talks to a fresh peer.
fn owned_leg(hub: &mut MediaHub, key: &CallKey) -> Result<SocketPair, MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    hub.attach(
        key.clone(),
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    Ok(peer)
}

#[tokio::test]
async fn attaching_a_protocol_leg_starts_no_task() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let key = key()?;
    let _peer = owned_leg(&mut hub, &key)?;
    let started = hub.workers.len();
    hub.close().await?;
    assert_eq!(started, 0);
    Ok(())
}

// Sends one audio packet from a peer to the leg that it talks to.
async fn speak(
    peer: &SocketPair,
    hub: &MediaHub,
    key: &CallKey,
    sequence: u16,
) -> Result<(), MediaError> {
    let local = hub
        .legs
        .get(key)
        .ok_or(MediaError::UnknownStream)?
        .local_addr()?;
    let packet =
        rusterisk_media::RtpPacket::new(0, sequence, u32::from(sequence) * 160, 7, &[0x55; 160])?;
    peer.data()
        .send_to(&packet.encode()?, local)
        .await
        .map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn one_failing_leg_ends_while_another_keeps_its_audio() -> Result<(), MediaError> {
    let (_handle, mut hub, mut notices) = MediaHub::new(4);
    let failing = named("failing")?;
    let kept = named("kept")?;
    let failing_peer = owned_leg(&mut hub, &failing)?;
    let kept_peer = owned_leg(&mut hub, &kept)?;
    hub.command(Command::Bridge {
        id: 1,
        members: Vec::from([failing.clone(), kept.clone()]),
    })?;
    let other_family =
        std::net::UdpSocket::bind((std::net::Ipv6Addr::LOCALHOST, 0)).map_err(failure)?;
    other_family.set_nonblocking(true).map_err(failure)?;
    hub.legs
        .get_mut(&failing)
        .ok_or(MediaError::UnknownStream)?
        .replace_data_socket(other_family);
    speak(&kept_peer, &hub, &kept, 1).await?;
    received(&mut hub, &kept, 1).await?;
    // The failing leg can fail in a turn before the kept leg connects, so the notices come in either order.
    let mut connected = false;
    let mut stopped = false;
    while let Ok(notice) = notices.try_recv() {
        connected |= matches!(&notice, MediaNotice::Connected { key } if *key == kept);
        stopped |=
            matches!(&notice, MediaNotice::Stopped { key, result: Err(_) } if *key == failing);
    }
    assert!(
        connected && stopped,
        "connected {connected}, stopped {stopped}"
    );
    assert!(!hub.ports.contains_key(&failing));
    speak(&kept_peer, &hub, &kept, 2).await?;
    let result = received(&mut hub, &kept, 2).await;
    drop(failing_peer);
    hub.close().await?;
    result
}

// Runs media turns until the leg has read the given number of packets.
// A loopback datagram can reach its socket after the sender returns, so one turn may come too early.
async fn received(hub: &mut MediaHub, key: &CallKey, count: u64) -> Result<(), MediaError> {
    let socket = hub
        .legs
        .get(key)
        .ok_or(MediaError::UnknownStream)?
        .input_observer()?;
    for _ in 0..100 {
        hub.tick()?;
        let read = hub
            .legs
            .get(key)
            .ok_or(MediaError::UnknownStream)?
            .measurements()
            .local
            .rxcount;
        if read >= count {
            return Ok(());
        }
        Wait::Ready
            .bounded(socket.peek_from(&mut [0; 1]))
            .await
            .map_err(failure)?
            .map_err(failure)?;
    }
    Err(MediaError::Transport(
        "the leg never read the packet".into(),
    ))
}

#[tokio::test]
async fn stream_answers_and_digits_remain_available_during_notice_pressure(
) -> Result<(), MediaError> {
    let (_handle, mut hub, mut notices) = MediaHub::new(2);
    let key = key()?;
    let (mut port, link) = Port::new(
        PeerMediaDirection::SendReceive,
        DigitSupport::Unsupported,
        hub.notices.reserve_leg()?,
    );
    let (answer, answers) = mpsc::channel(1);
    port.answers = Some(answers);
    hub.install_link(key.clone(), port, Gate::Open, 8000)?;
    for _ in 0..hub.notices.ordinary.max_capacity() {
        hub.notice(MediaNotice::Answer {
            key: named("filler")?,
        })?;
    }
    let tone = DtmfTone {
        digit: rusterisk_core::DtmfDigit::Hash,
        duration_ms: 120,
    };
    answer.try_send(()).map_err(failure)?;
    link.digits.try_send(tone).map_err(failure)?;
    hub.tick()?;
    while let Ok(notice) = notices.try_recv() {
        assert!(
            matches!(notice, MediaNotice::Answer { key: filler } if filler.id.as_str() == "filler")
        );
    }
    hub.tick()?;
    assert!(matches!(notices.try_recv(), Ok(MediaNotice::Answer { key: source }) if source == key));
    assert!(
        matches!(notices.try_recv(), Ok(MediaNotice::Dtmf { key: source, tone: received }) if source == key && received == tone)
    );
    assert!(notices.try_recv().is_err());
    hub.close().await
}
