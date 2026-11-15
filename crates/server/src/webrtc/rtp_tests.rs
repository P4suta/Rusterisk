use super::{negotiation_error, Engine, WebRtcOffer, WebRtcSession};
use crate::rtp::SocketPair;
use rusterisk_config::Codec;
use rusterisk_media::{mulaw_encode, MediaError};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
use str0m::{
    net::{Receive, Transmit},
    rtp::{RtpPacket, RtpWrite},
    Event, Input, Output,
};

fn poll(engine: &mut Engine, received: &mut Vec<RtpPacket>) -> Result<(), MediaError> {
    for _ in 0..256 {
        match engine.rtc.poll_output().map_err(negotiation_error)? {
            Output::Timeout(timeout) => {
                engine.timeout = timeout;
                return Ok(());
            }
            Output::Transmit(transmit) => {
                if engine.transmits.len() >= 32 || transmit.contents.len() > 2048 {
                    return Err(MediaError::Capacity);
                }
                engine.transmits.push(transmit);
            }
            Output::Event(event) => {
                if let Event::RtpPacket(packet) = event {
                    if received.len() >= 4 {
                        return Err(MediaError::Capacity);
                    }
                    received.push(packet);
                }
            }
        }
    }
    Err(MediaError::Capacity)
}

fn deliver(
    receiver: &mut Engine,
    transmit: &Transmit,
    contents: &[u8],
    now: Instant,
    received: &mut Vec<RtpPacket>,
) -> Result<(), MediaError> {
    receiver
        .rtc
        .handle_input(Input::Receive(
            now,
            Receive {
                proto: transmit.proto,
                source: transmit.source,
                destination: transmit.destination,
                contents: contents.try_into().map_err(negotiation_error)?,
            },
        ))
        .map_err(negotiation_error)?;
    poll(receiver, received)
}

fn transfer(
    sender: &mut Engine,
    receiver: &mut Engine,
    now: Instant,
    received: &mut Vec<RtpPacket>,
) -> Result<(), MediaError> {
    let transmits = std::mem::replace(&mut sender.transmits, Vec::with_capacity(32));
    for transmit in transmits {
        deliver(receiver, &transmit, &transmit.contents, now, received)?;
    }
    Ok(())
}

fn advance(
    left: &mut Engine,
    right: &mut Engine,
    now: Instant,
    received: &mut Vec<RtpPacket>,
) -> Result<(), MediaError> {
    for engine in [&mut *left, &mut *right] {
        if engine.timeout <= now {
            engine
                .rtc
                .handle_input(Input::Timeout(now))
                .map_err(negotiation_error)?;
            poll(engine, received)?;
        }
    }
    transfer(left, right, now, received)?;
    transfer(right, left, now, received)
}

#[tokio::test]
async fn authenticated_packets_preserve_source_sequence_timestamp_and_marker(
) -> Result<(), MediaError> {
    let first = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let second = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(negotiation_error)?;
    let mut now = Instant::now();
    let local = first.local_addr().map_err(negotiation_error)?;
    let offer = WebRtcOffer::new(first, local, &[Codec::Pcmu], now)?;
    let local = second.local_addr().map_err(negotiation_error)?;
    let (mut sender, answer) =
        WebRtcSession::answer(second, local, offer.description(), &[Codec::Pcmu], now)?;
    let mut receiver = offer.accept_answer(&answer)?;
    let mut received = Vec::with_capacity(4);
    for _ in 0..1000 {
        now += Duration::from_millis(10);
        advance(&mut receiver.engine, &mut sender.engine, now, &mut received)?;
        if receiver.engine.rtc.is_connected() && sender.engine.rtc.is_connected() {
            break;
        }
    }
    assert!(receiver.engine.rtc.is_connected());
    assert!(sender.engine.rtc.is_connected());
    let mid = sender.engine.mid.ok_or(MediaError::InvalidRoute)?;
    let source = {
        let mut direct = sender.engine.rtc.direct_api();
        let stream = direct
            .stream_tx_by_mid(mid, None)
            .ok_or(MediaError::InvalidRoute)?;
        let source = stream.ssrc();
        stream.write_rtp(
            RtpWrite::new(
                sender.payload,
                17_u64.into(),
                0x1020_3040,
                now,
                vec![mulaw_encode(4000); 160],
            )
            .marker(true),
        );
        source
    };
    poll(&mut sender.engine, &mut received)?;
    let mut encrypted = None;
    for _ in 0..100 {
        if let Some(index) = sender.engine.transmits.iter().position(|transmit| {
            let bytes = &transmit.contents;
            bytes.len() >= 12 && bytes[0] >> 6 == 2 && bytes[1] & 127 == *sender.payload
        }) {
            encrypted = Some(sender.engine.transmits.remove(index));
            break;
        }
        now += Duration::from_millis(10);
        if sender.engine.timeout <= now {
            sender
                .engine
                .rtc
                .handle_input(Input::Timeout(now))
                .map_err(negotiation_error)?;
            poll(&mut sender.engine, &mut received)?;
        }
    }
    let packet = encrypted
        .ok_or_else(|| MediaError::Transport("Expected encrypted audio datagram".into()))?;
    let mut corrupted = packet.contents.to_vec();
    let tag = corrupted.last_mut().ok_or(MediaError::InvalidPacket)?;
    *tag ^= 1;
    deliver(
        &mut receiver.engine,
        &packet,
        &corrupted,
        now,
        &mut received,
    )?;
    assert!(received.is_empty());
    deliver(
        &mut receiver.engine,
        &packet,
        &packet.contents,
        now,
        &mut received,
    )?;
    for _ in 0..20 {
        if !received.is_empty() {
            break;
        }
        now += Duration::from_millis(10);
        advance(&mut receiver.engine, &mut sender.engine, now, &mut received)?;
    }
    let received = received
        .pop()
        .ok_or_else(|| MediaError::Transport("Expected authenticated RTP metadata".into()))?;
    assert_eq!(received.header.ssrc, source);
    assert_eq!(received.header.sequence_number, 17);
    assert_eq!(received.header.timestamp, 0x1020_3040);
    assert!(received.header.marker);
    assert_eq!(received.payload.as_ref(), &[mulaw_encode(4000); 160]);
    Ok(())
}
