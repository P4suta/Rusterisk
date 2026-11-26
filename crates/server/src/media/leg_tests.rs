use super::*;
use rusterisk_api::ChannelId;
use rusterisk_core::{DtmfDigit, DtmfOutbox, DtmfTiming};
use rusterisk_media::{AudioFormat, ControlKind, RtpPacket};
use rusterisk_test_support::Wait;
use std::net::Ipv4Addr;
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn key(name: &str) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(name.to_owned()).map_err(failure)?,
        generation: 1,
    })
}

// Attaches one protocol leg, with telephone events when the session offers them.
fn leg(
    hub: &mut MediaHub,
    key: &CallKey,
    telephone: Option<TelephoneFormat>,
) -> Result<SocketPair, MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    hub.attach(
        key.clone(),
        SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone,
        },
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    Ok(peer)
}

fn read(hub: &MediaHub, key: &CallKey) -> Result<u64, MediaError> {
    Ok(hub
        .legs
        .get(key)
        .ok_or(MediaError::UnknownStream)?
        .measurements()
        .local
        .rxcount)
}

// Signaling bridges two legs, and a peer speaks before the media task reads its commands.
// The bridge applies before the turn reads that audio, so the other peer hears it.
#[tokio::test]
async fn a_bridge_queued_before_a_turn_carries_that_turns_audio() -> Result<(), MediaError> {
    let (handle, mut hub, _notices) = MediaHub::new(4);
    let alice = key("alice")?;
    let bob = key("bob")?;
    let alice_peer = leg(&mut hub, &alice, None)?;
    let bob_peer = leg(&mut hub, &bob, None)?;
    handle.bridge(1, Vec::from([alice.clone(), bob.clone()]))?;
    let local = hub
        .legs
        .get(&bob)
        .ok_or(MediaError::UnknownStream)?
        .local_addr()?;
    let mut sender = rusterisk_media::RtpSender::new(AudioFormat::Pcmu, 0, [8; 10])?;
    let spoken = sender.packet(rusterisk_media::Frame::new(&[-7000; 160], 8000)?)?;
    bob_peer
        .data()
        .send_to(&spoken.encode()?, local)
        .await
        .map_err(failure)?;
    // A loopback datagram can reach its socket after the sender returns.
    let input = hub
        .legs
        .get(&bob)
        .ok_or(MediaError::UnknownStream)?
        .input_observer()?;
    Wait::Ready
        .bounded(input.readable())
        .await
        .map_err(failure)?
        .map_err(failure)?;
    hub.tick()?;
    let mut bytes = [0; 1501];
    let (size, _) = Wait::Ready
        .bounded(alice_peer.data().recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let heard = RtpPacket::parse(&bytes[..size])?;
    hub.close().await?;
    assert!(heard
        .payload()
        .iter()
        .all(|byte| (i32::from(rusterisk_media::mulaw_decode(*byte)) + 7000).abs() < 300));
    Ok(())
}

// A G.711 leg held this many bytes inline and in reserved memory when the budget took effect.
const LEG_HELD: usize = 6608;
// A leg task held two frame channels, and a Tokio channel reserves a block of 32 slots on its first send.
const TASK_CHANNELS: usize = 2 * 32 * std::mem::size_of::<rusterisk_media::Frame>();

#[tokio::test]
async fn the_socket_to_socket_g711_turn_allocates_nothing_after_warmup() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(2);
    let left = key("left")?;
    let right = key("right")?;
    let mut peers = Vec::new();
    for (key, sample) in [(&left, 4000_i16), (&right, -7000)] {
        let socket = leg(&mut hub, key, None)?;
        let input = hub
            .legs
            .get(key)
            .ok_or(MediaError::UnknownStream)?
            .input_observer()?;
        let sender = rusterisk_media::RtpSender::new(AudioFormat::Pcmu, 0, [3; 10])?;
        peers.push((socket, input, sender, sample));
    }
    hub.mixer.bridge(1, &[left, right])?;
    for turn in 0..50 {
        let mut bytes = [0; 1501];
        for (peer, input, sender, sample) in &mut peers {
            let packet = sender
                .packet(Frame::new(&[*sample; 160], 8000)?)?
                .encode()?;
            peer.data()
                .send_to(&packet, input.local_addr().map_err(failure)?)
                .await
                .map_err(failure)?;
            Wait::Ready
                .bounded(input.peek_from(&mut bytes))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        }
        let mut result = Ok(());
        let counted = allocation_counter::measure(|| {
            result = hub.tick();
        });
        result?;
        if turn > 0 {
            assert_eq!(counted.count_total, 0, "turn {turn}: {counted:?}");
        }
        for ((peer, _, _, _), expected) in peers.iter().zip([-7000, 4000]) {
            let (size, _) = Wait::Ready
                .bounded(peer.data().recv_from(&mut bytes))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            let packet = RtpPacket::parse(&bytes[..size])?;
            assert_eq!(
                packet.payload(),
                [rusterisk_media::mulaw_encode(expected); 160]
            );
        }
    }
    hub.close().await
}

#[tokio::test]
async fn a_g711_leg_stays_within_its_memory_budget() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let measured = key("measured")?;
    let _peer = leg(&mut hub, &measured, None)?;
    let owned = hub.legs.get(&measured).ok_or(MediaError::UnknownStream)?;
    let held = std::mem::size_of::<RtpLeg>() + owned.reserved_bytes();
    hub.close().await?;
    const { assert!(TASK_CHANNELS > LEG_HELD * 5 / 4) };
    assert!(held <= LEG_HELD * 5 / 4, "a G.711 leg holds {held} bytes");
    assert!(
        held * 10 >= LEG_HELD * 5 / 4 * 6,
        "a G.711 leg holds {held} bytes, so the budget can shrink"
    );
    Ok(())
}

#[tokio::test]
async fn attaching_a_webrtc_leg_starts_no_task() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let offer = crate::webrtc::WebRtcOffer::new(
        sockets,
        local,
        &[rusterisk_config::Codec::Pcmu],
        std::time::Instant::now(),
    )?;
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (answer, _description) = crate::webrtc::WebRtcSession::answer(
        sockets,
        local,
        offer.description(),
        &[rusterisk_config::Codec::Pcmu],
        std::time::Instant::now(),
    )?;
    let browser = key("browser")?;
    hub.attach_webrtc(
        browser.clone(),
        answer,
        PeerMediaDirection::SendReceive,
        AttachmentMode::Answered,
    )?;
    let started = hub.workers.len();
    let owned = hub.webrtc_legs.contains_key(&browser);
    hub.close().await?;
    assert_eq!(started, 0);
    assert!(owned);
    Ok(())
}

// A peer floods a leg, and each turn reads one reorder window of its datagrams.
#[tokio::test]
async fn a_leg_reads_at_most_one_window_of_datagrams_in_a_turn() -> Result<(), MediaError> {
    let (_handle, mut hub, _notices) = MediaHub::new(4);
    let flooded = key("flooded")?;
    let peer = leg(&mut hub, &flooded, None)?;
    let local = hub
        .legs
        .get(&flooded)
        .ok_or(MediaError::UnknownStream)?
        .local_addr()?;
    for sequence in 0..40_u16 {
        let packet = RtpPacket::new(0, sequence, u32::from(sequence) * 160, 7, &[0x55; 160])?;
        peer.data()
            .send_to(&packet.encode()?, local)
            .await
            .map_err(failure)?;
    }
    // A loopback datagram can reach its socket after the sender returns.
    let input = hub
        .legs
        .get(&flooded)
        .ok_or(MediaError::UnknownStream)?
        .input_observer()?;
    Wait::Ready
        .bounded(input.readable())
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let mut counts = Vec::with_capacity(3);
    for _ in 0..3 {
        hub.tick()?;
        counts.push(read(&hub, &flooded)?);
    }
    hub.close().await?;
    assert_eq!(counts, [16, 32, 40]);
    Ok(())
}

// Signaling sends a digit sequence to a leg that the media task owns.
#[tokio::test(start_paused = true)]
async fn an_owned_leg_sends_digits_and_reports_their_finish() -> Result<(), MediaError> {
    let (_handle, mut hub, mut notices) = MediaHub::new(4);
    let dialing = key("dialing")?;
    let format = TelephoneFormat::new(101, 8000)?;
    let peer = leg(&mut hub, &dialing, Some(format))?;
    let timing = DtmfTiming::new(0, 20, 60, 0).map_err(|_| MediaError::InvalidRoute)?;
    let command = DtmfOutbox::new(u16::MAX)
        .and_then(|mut outbox| outbox.admit(&[DtmfDigit::Five], timing))
        .map_err(|_| MediaError::InvalidRoute)?;
    let token = command.token;
    let (reply, mut response) = oneshot::channel();
    hub.command(Command::Digits {
        key: dialing.clone(),
        command: RtpDigitCommand::Sequence(command),
        reply: Some(reply),
    })?;
    assert!(matches!(response.try_recv(), Ok(Ok(()))));
    let mut finished = false;
    for _ in 0..50 {
        hub.tick()?;
        while let Ok(notice) = notices.try_recv() {
            finished |= matches!(notice, MediaNotice::DtmfFinished { key, token: done } if key == dialing && done == token);
        }
        if finished {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    // The leg also sends the mixer's audio, so the peer looks for the first telephone event.
    let mut bytes = [0; 1501];
    let mut events = 0;
    while let Ok(Ok((size, _))) = Wait::Quiet.bounded(peer.data().recv_from(&mut bytes)).await {
        events += usize::from(RtpPacket::parse(&bytes[..size])?.payload_type() == 101);
    }
    hub.command(Command::Release { key: dialing })?;
    let (size, _) = Wait::Ready
        .bounded(peer.control().recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let goodbye = rusterisk_media::inspect_rtcp(&bytes[..size])?;
    hub.close().await?;
    assert!(finished, "the leg never reported the finished digits");
    assert!(events > 0, "the peer received no telephone event");
    assert_eq!(goodbye.last(), Some(&ControlKind::Goodbye));
    Ok(())
}
