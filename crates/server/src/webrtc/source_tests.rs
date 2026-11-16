use super::{
    negotiation_error,
    rtp_tests::{advance, connected, deliver, poll},
    WebRtcOffer, WebRtcSession,
};
use crate::rtp::SocketPair;
use rusterisk_config::Codec;
use rusterisk_media::MediaError;
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use str0m::{
    media::Pt,
    net::Transmit,
    rtp::{RtpPacket, RtpWrite, Ssrc},
    Input,
};

fn sources(sender: &mut WebRtcSession) -> Result<(Ssrc, Ssrc), MediaError> {
    let mid = sender.engine.mid.ok_or(MediaError::InvalidRoute)?;
    let source = sender
        .engine
        .rtc
        .direct_api()
        .stream_tx_by_mid(mid, None)
        .ok_or(MediaError::InvalidRoute)?
        .ssrc();
    let separate = (*source).wrapping_add(1).max(1).into();
    Ok((source, separate))
}

fn write(
    sender: &mut WebRtcSession,
    source: Ssrc,
    payload: Pt,
    sequence: u64,
    now: Instant,
) -> Result<(), MediaError> {
    let mid = sender.engine.mid.ok_or(MediaError::InvalidRoute)?;
    let bytes = if *payload == 101 {
        [11, 0x8a, 0, 160].into_iter().collect()
    } else {
        vec![0x8f; 160]
    };
    sender
        .engine
        .rtc
        .direct_api()
        .declare_stream_tx(source, None, mid, None)
        .write_rtp(RtpWrite::new(
            payload,
            sequence.into(),
            sequence as u32 * 160,
            now,
            bytes,
        ));
    Ok(())
}

fn pump(
    receiver: &mut WebRtcSession,
    sender: &mut WebRtcSession,
    now: &mut Instant,
    received: &mut Vec<RtpPacket>,
    count: usize,
) -> Result<(), MediaError> {
    for _ in 0..100 {
        *now += Duration::from_millis(10);
        advance(&mut receiver.engine, &mut sender.engine, *now, received)?;
        if received.len() >= count {
            break;
        }
    }
    Ok(())
}

fn encrypted(
    sender: &mut WebRtcSession,
    now: &mut Instant,
    source: Ssrc,
    payload: Pt,
) -> Result<Transmit, MediaError> {
    let mut received = Vec::with_capacity(4);
    for _ in 0..100 {
        poll(&mut sender.engine, &mut received)?;
        let source = (*source).to_be_bytes();
        if let Some(index) = sender.engine.transmits.iter().position(|transmit| {
            let bytes = &transmit.contents;
            bytes.len() >= 12
                && bytes[0] >> 6 == 2
                && bytes[1] & 127 == *payload
                && bytes[8..12] == source
        }) {
            return Ok(sender.engine.transmits.remove(index));
        }
        *now += Duration::from_millis(10);
        if sender.engine.timeout <= *now {
            sender
                .engine
                .rtc
                .handle_input(Input::Timeout(*now))
                .map_err(negotiation_error)?;
        }
    }
    Err(MediaError::Transport(
        "Expected encrypted source packet".into(),
    ))
}

#[tokio::test]
async fn simultaneous_sources_with_one_mid_and_no_rid_leave_both_send_queues(
) -> Result<(), MediaError> {
    let (_receiver, mut sender, mut now) = connected()?;
    let (audio, telephone) = sources(&mut sender)?;
    let payload = sender.payload;
    write(&mut sender, audio, payload, 17, now)?;
    write(&mut sender, telephone, 101.into(), 1, now)?;
    let mut observed = Vec::with_capacity(2);
    let mut received = Vec::with_capacity(4);
    for _ in 0..100 {
        poll(&mut sender.engine, &mut received)?;
        for transmit in sender.engine.transmits.drain(..) {
            let bytes = &transmit.contents;
            if bytes.len() >= 12 && bytes[0] >> 6 == 2 && matches!(bytes[1] & 127, 0 | 101) {
                let source =
                    u32::from_be_bytes(bytes[8..12].try_into().map_err(negotiation_error)?);
                if !observed.contains(&source) {
                    if observed.len() >= 2 {
                        return Err(MediaError::Capacity);
                    }
                    observed.push(source);
                }
            }
        }
        if observed.len() == 2 {
            break;
        }
        now += Duration::from_millis(10);
        if sender.engine.timeout <= now {
            sender
                .engine
                .rtc
                .handle_input(Input::Timeout(now))
                .map_err(negotiation_error)?;
        }
    }
    assert!(
        observed.contains(&*audio),
        "Audio source stayed queued: {observed:?}"
    );
    assert!(
        observed.contains(&*telephone),
        "Telephone source stayed queued: {observed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn authenticated_sources_with_one_mid_remain_receivable_when_packets_alternate(
) -> Result<(), MediaError> {
    let (mut receiver, mut sender, mut now) = connected()?;
    let (audio, telephone) = sources(&mut sender)?;
    let payload = sender.payload;
    let mut received = Vec::with_capacity(4);
    write(&mut sender, audio, payload, 17, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 1)?;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].header.ssrc, audio);
    let original_time = receiver
        .engine
        .rtc
        .direct_api()
        .stream_rx(&audio)
        .and_then(|stream| stream.last_time())
        .ok_or(MediaError::InvalidPacket)?;
    assert!(sender.engine.rtc.direct_api().remove_stream_tx(audio));
    write(&mut sender, telephone, 101.into(), 1, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 2)?;
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header.ssrc, telephone);
    assert_eq!(
        receiver
            .engine
            .rtc
            .direct_api()
            .stream_rx(&audio)
            .and_then(|stream| stream.last_time()),
        Some(original_time),
        "A second source discarded the original authenticated source state"
    );
    assert!(sender.engine.rtc.direct_api().remove_stream_tx(telephone));
    write(&mut sender, audio, payload, 18, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 3)?;
    assert_eq!(
        received.len(),
        3,
        "An authenticated source disappeared after another source arrived"
    );
    assert_eq!(received[2].header.ssrc, audio);
    assert_eq!(received[2].header.sequence_number, 18);
    Ok(())
}

#[tokio::test]
async fn unauthenticated_sources_cannot_replace_an_authenticated_source() -> Result<(), MediaError>
{
    let (mut receiver, mut sender, mut now) = connected()?;
    let (audio, telephone) = sources(&mut sender)?;
    let payload = sender.payload;
    let mut received = Vec::with_capacity(4);
    write(&mut sender, audio, payload, 17, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 1)?;
    let original_time = receiver
        .engine
        .rtc
        .direct_api()
        .stream_rx(&audio)
        .and_then(|stream| stream.last_time())
        .ok_or(MediaError::InvalidPacket)?;
    assert!(sender.engine.rtc.direct_api().remove_stream_tx(audio));
    write(&mut sender, telephone, 101.into(), 1, now)?;
    let packet = encrypted(&mut sender, &mut now, telephone, 101.into())?;
    let mut corrupt = packet.contents.to_vec();
    *corrupt.last_mut().ok_or(MediaError::InvalidPacket)? ^= 1;
    deliver(&mut receiver.engine, &packet, &corrupt, now, &mut received)?;
    assert_eq!(received.len(), 1);
    assert_eq!(
        receiver
            .engine
            .rtc
            .direct_api()
            .stream_rx(&audio)
            .and_then(|stream| stream.last_time()),
        Some(original_time),
        "An authentication failure replaced an admitted source"
    );
    assert!(receiver
        .engine
        .rtc
        .direct_api()
        .stream_rx(&telephone)
        .is_none());
    assert!(sender.engine.rtc.direct_api().remove_stream_tx(telephone));
    write(&mut sender, audio, payload, 18, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 2)?;
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header.ssrc, audio);
    Ok(())
}

#[tokio::test]
async fn receiving_sources_share_a_fixed_budget_without_evicting_admitted_sources(
) -> Result<(), MediaError> {
    let (mut receiver, mut sender, mut now) = connected()?;
    let (audio, _) = sources(&mut sender)?;
    let payload = sender.payload;
    let mut previous = audio;
    let mut received = Vec::with_capacity(4);
    let mut admitted = Vec::with_capacity(4);
    for offset in 0..5 {
        let source = match offset {
            0 => audio,
            offset => if *audio > u32::MAX - 8 {
                *audio - offset
            } else {
                *audio + offset
            }
            .into(),
        };
        if offset > 0 {
            assert!(sender.engine.rtc.direct_api().remove_stream_tx(previous));
        }
        write(&mut sender, source, payload, 17, now)?;
        let packet = encrypted(&mut sender, &mut now, source, payload)?;
        deliver(
            &mut receiver.engine,
            &packet,
            &packet.contents,
            now,
            &mut received,
        )?;
        if offset < 4 {
            let packet = received.pop().ok_or(MediaError::InvalidPacket)?;
            assert_eq!(packet.header.ssrc, source);
            admitted.push(source);
        } else {
            assert!(
                received.is_empty(),
                "The receive-source budget was exceeded"
            );
            assert!(receiver
                .engine
                .rtc
                .direct_api()
                .stream_rx(&source)
                .is_none());
        }
        for source in &admitted {
            assert!(
                receiver
                    .engine
                    .rtc
                    .direct_api()
                    .stream_rx(source)
                    .and_then(|stream| stream.last_time())
                    .is_some(),
                "An admitted source was evicted"
            );
        }
        previous = source;
    }
    Ok(())
}

#[tokio::test]
async fn announced_sources_share_the_receive_budget_before_transport_admission(
) -> Result<(), MediaError> {
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let local = sockets.local_addr().map_err(negotiation_error)?;
    let offer = WebRtcOffer::new(sockets, local, &[Codec::Pcmu], Instant::now())?;
    let mut description = offer.description().to_owned();
    for source in 1..=3 {
        description.push_str(&format!("a=ssrc:{source} cname:source-budget\r\n"));
    }
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let local = sockets.local_addr().map_err(negotiation_error)?;
    WebRtcSession::answer(sockets, local, &description, &[Codec::Pcmu], Instant::now())?;
    for source in 4..=5 {
        description.push_str(&format!("a=ssrc:{source} cname:source-budget\r\n"));
    }
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let local = sockets.local_addr().map_err(negotiation_error)?;
    match WebRtcSession::answer(sockets, local, &description, &[Codec::Pcmu], Instant::now()) {
        Err(_) => Ok(()),
        Ok(_) => Err(MediaError::Transport(
            "Source declarations exceeded the receive budget".into(),
        )),
    }
}

#[tokio::test]
async fn independent_sources_preserve_reordered_packets_replay_windows_and_rollover(
) -> Result<(), MediaError> {
    let (mut receiver, mut sender, mut now) = connected()?;
    let (audio, telephone) = sources(&mut sender)?;
    let payload = sender.payload;
    let mut received = Vec::with_capacity(4);
    write(&mut sender, audio, payload, 65_534, now)?;
    let late = encrypted(&mut sender, &mut now, audio, payload)?;
    write(&mut sender, audio, payload, 65_535, now)?;
    let first = encrypted(&mut sender, &mut now, audio, payload)?;
    deliver(
        &mut receiver.engine,
        &first,
        &first.contents,
        now,
        &mut received,
    )?;
    write(&mut sender, telephone, 101.into(), 1, now)?;
    let separate = encrypted(&mut sender, &mut now, telephone, 101.into())?;
    deliver(
        &mut receiver.engine,
        &separate,
        &separate.contents,
        now,
        &mut received,
    )?;
    deliver(
        &mut receiver.engine,
        &late,
        &late.contents,
        now,
        &mut received,
    )?;
    assert_eq!(received.len(), 3);
    assert_eq!(*received[0].seq_no, 65_535);
    assert_eq!(*received[1].seq_no, 1);
    assert_eq!(*received[2].seq_no, 65_534);
    for replay in [&late, &first, &separate] {
        deliver(
            &mut receiver.engine,
            replay,
            &replay.contents,
            now,
            &mut received,
        )?;
        assert_eq!(received.len(), 3, "An authenticated packet was replayed");
    }
    write(&mut sender, audio, payload, 65_536, now)?;
    let wrapped = encrypted(&mut sender, &mut now, audio, payload)?;
    deliver(
        &mut receiver.engine,
        &wrapped,
        &wrapped.contents,
        now,
        &mut received,
    )?;
    assert_eq!(received.len(), 4);
    assert_eq!(received[3].header.ssrc, audio);
    assert_eq!(received[3].header.sequence_number, 0);
    assert_eq!(*received[3].seq_no, 65_536);
    Ok(())
}

#[tokio::test]
async fn failed_authentication_does_not_consume_receive_source_slots() -> Result<(), MediaError> {
    let (mut receiver, mut sender, mut now) = connected()?;
    let (audio, _) = sources(&mut sender)?;
    let payload = sender.payload;
    let mut received = Vec::with_capacity(4);
    write(&mut sender, audio, payload, 17, now)?;
    pump(&mut receiver, &mut sender, &mut now, &mut received, 1)?;
    let original_time = receiver
        .engine
        .rtc
        .direct_api()
        .stream_rx(&audio)
        .and_then(|stream| stream.last_time())
        .ok_or(MediaError::InvalidPacket)?;
    let mut valid = None;
    let mut previous = audio;
    for offset in 1..=8 {
        assert!(sender.engine.rtc.direct_api().remove_stream_tx(previous));
        let source = if *audio > u32::MAX - 16 {
            *audio - offset
        } else {
            *audio + offset
        }
        .into();
        write(&mut sender, source, 101.into(), 1, now)?;
        let packet = encrypted(&mut sender, &mut now, source, 101.into())?;
        let mut corrupt = packet.contents.to_vec();
        *corrupt.last_mut().ok_or(MediaError::InvalidPacket)? ^= 1;
        deliver(&mut receiver.engine, &packet, &corrupt, now, &mut received)?;
        assert_eq!(received.len(), 1);
        assert!(receiver
            .engine
            .rtc
            .direct_api()
            .stream_rx(&source)
            .is_none());
        assert_eq!(
            receiver
                .engine
                .rtc
                .direct_api()
                .stream_rx(&audio)
                .and_then(|stream| stream.last_time()),
            Some(original_time)
        );
        if offset == 1 {
            valid = Some(packet);
        }
        previous = source;
    }
    let valid = valid.ok_or(MediaError::InvalidPacket)?;
    deliver(
        &mut receiver.engine,
        &valid,
        &valid.contents,
        now,
        &mut received,
    )?;
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].header.payload_type, 101.into());
    Ok(())
}

fn answer_with_sources(extra: u32) -> Result<(WebRtcOffer, String), MediaError> {
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let local = sockets.local_addr().map_err(negotiation_error)?;
    let offer = WebRtcOffer::new(sockets, local, &[Codec::Pcmu], Instant::now())?;
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let local = sockets.local_addr().map_err(negotiation_error)?;
    let (_peer, mut answer) = WebRtcSession::answer(
        sockets,
        local,
        offer.description(),
        &[Codec::Pcmu],
        Instant::now(),
    )?;
    for source in 1..=extra {
        answer.push_str(&format!("a=ssrc:{source} cname:source-budget\r\n"));
    }
    Ok((offer, answer))
}

#[tokio::test]
async fn answer_declarations_share_the_receive_budget_before_transport_admission(
) -> Result<(), MediaError> {
    let (offer, answer) = answer_with_sources(3)?;
    offer.accept_answer(&answer)?;
    let (offer, answer) = answer_with_sources(5)?;
    match offer.accept_answer(&answer) {
        Err(_) => Ok(()),
        Ok(_) => Err(MediaError::Transport(
            "Answer declarations exceeded the receive budget".into(),
        )),
    }
}
