#[path = "../../tests/rtp_leg/driver.rs"]
mod driver;

use super::{
    ControlReception, LinkStats, Measurements, RtpChannels, RtpLeg, RtpSession, SocketPair,
};
use driver::RtpLink;
use rusterisk_core::PeerMediaDirection;
use rusterisk_media::MAX_PACKET;
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, MediaError, RtpPacket, RtpSender};
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

async fn finish(
    mut task: tokio::task::JoinHandle<Result<LinkStats, MediaError>>,
) -> Result<LinkStats, MediaError> {
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

struct Fixture {
    local: SocketAddr,
    control: SocketAddr,
    link: RtpLink,
    received: mpsc::Receiver<Frame>,
    outgoing: mpsc::Sender<Frame>,
    stop: watch::Sender<bool>,
    _direction: watch::Sender<PeerMediaDirection>,
}

fn fixture(peer: &SocketPair) -> Result<Fixture, MediaError> {
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let control = sockets.control_addr().map_err(failure)?;
    let (incoming, received) = mpsc::channel(3);
    let (digits, _digits) = mpsc::channel(16);
    let (outgoing, output) = mpsc::channel(3);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (directed, direction) = watch::channel(PeerMediaDirection::SendReceive);
    let link = RtpLink::new(
        sockets,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: None,
        },
        RtpChannels {
            incoming,
            digits,
            outgoing: output,
            commands,
            completed,
            stop: stopped,
            direction,
        },
    )?;
    Ok(Fixture {
        local,
        control,
        link,
        received,
        outgoing,
        stop,
        _direction: directed,
    })
}

#[tokio::test]
async fn fixed_audio_frames_keep_their_sample_clock_across_transport_scheduling_jitter(
) -> Result<(), MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let mut fixture = fixture(&peer)?;
    fixture.link.leg.sender = RtpSender::new(AudioFormat::Pcmu, 0, [255; 10])?;
    let frame = Frame::new(&[4000; 160], 8000)?;
    let first = fixture.link.leg.audio_packet(frame, 0)?;
    for (index, now) in [21, 39, 61, 80, 101].into_iter().enumerate() {
        let packet = fixture.link.leg.audio_packet(frame, now)?;
        assert_eq!(packet.ssrc(), first.ssrc());
        assert_eq!(
            packet.sequence(),
            first.sequence().wrapping_add(index as u16 + 1)
        );
        assert_eq!(
            packet.timestamp(),
            first.timestamp().wrapping_add(160 * (index as u32 + 1))
        );
    }
    let resumed = fixture.link.leg.audio_packet(frame, 1001)?;
    assert_eq!(resumed.timestamp(), first.timestamp().wrapping_add(8000));
    Ok(())
}

#[tokio::test]
async fn rejected_audio_frames_and_clock_inputs_do_not_advance_output_packets(
) -> Result<(), MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let mut fixture = fixture(&peer)?;
    let frame = Frame::new(&[4000; 160], 8000)?;
    let first = fixture.link.leg.audio_packet(frame, 0)?;
    assert!(matches!(
        fixture
            .link
            .leg
            .audio_packet(Frame::new(&[4000; 80], 8000)?, 20),
        Err(MediaError::InvalidFrame)
    ));
    let next = fixture.link.leg.audio_packet(frame, 20)?;
    assert_eq!(next.sequence(), first.sequence().wrapping_add(1));
    assert_eq!(next.timestamp(), first.timestamp().wrapping_add(160));
    assert!(matches!(
        fixture.link.leg.audio_packet(frame, 19),
        Err(MediaError::Transport(_))
    ));
    assert!(matches!(
        fixture.link.leg.audio_packet(frame, u64::MAX),
        Err(MediaError::Capacity)
    ));
    let next = fixture.link.leg.audio_packet(frame, 40)?;
    assert_eq!(next.sequence(), first.sequence().wrapping_add(2));
    assert_eq!(next.timestamp(), first.timestamp().wrapping_add(320));
    Ok(())
}

#[tokio::test]
async fn malformed_compound_packets_from_the_peer_leave_known_audio_running(
) -> Result<(), MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let Fixture {
        local,
        control,
        link,
        mut received,
        outgoing,
        stop,
        _direction,
    } = fixture(&peer)?;
    let (receipts, mut processed) = mpsc::channel(8);
    let task = tokio::spawn(
        link.serve(move |receipt| receipts.try_send(receipt).map_err(|_| MediaError::Capacity)),
    );
    let malformed: &[&[u8]] = &[
        &[0x80, 201, 0, 2, 0, 0, 0, 7],
        &[0x80, 201, 0, 1, 0, 0, 0, 7, 0x80, 202, 0, 3],
        &[0x80, 201, 0, 1, 0, 0, 0, 7, 0x40, 203, 0, 0],
        &[0xa0, 201, 0, 1, 0, 0, 0, 0],
        &[
            0x80, 201, 0, 1, 0, 0, 0, 7, 0x81, 202, 0, 2, 0, 0, 0, 7, 1, 7, b'a', 0,
        ],
    ];
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [7; 10])?;
        let mut observed = Vec::with_capacity(4);
        for stage in 0..2 {
            if stage == 1 {
                let valid = rusterisk_media::sender_report(7, 0, 0, 0, 0, "call")?;
                let size = peer
                    .control()
                    .send_to(&valid, control)
                    .await
                    .map_err(failure)?;
                if size != valid.len() {
                    return Err(MediaError::InvalidPacket);
                }
                if processed.recv().await.ok_or(MediaError::UnknownStream)?
                    != ControlReception::Accepted
                {
                    return Err(MediaError::InvalidPacket);
                }
                for packet in malformed {
                    let size = peer
                        .control()
                        .send_to(packet, control)
                        .await
                        .map_err(failure)?;
                    if size != packet.len() {
                        return Err(MediaError::InvalidPacket);
                    }
                }
                for _ in malformed {
                    let receipt = processed.recv().await.ok_or(MediaError::UnknownStream)?;
                    if receipt != ControlReception::Rejected {
                        return Err(MediaError::InvalidPacket);
                    }
                }
            }
            let input = [if stage == 0 { 4000 } else { -7000 }; 160];
            let bytes = sender.packet(Frame::new(&input, 8000)?)?.encode()?;
            peer.data().send_to(&bytes, local).await.map_err(failure)?;
            let incoming = received.recv().await.ok_or(MediaError::UnknownStream)?;
            observed.push(
                incoming.samples().len() == 160
                    && incoming
                        .samples()
                        .iter()
                        .all(|actual| (i32::from(*actual) - i32::from(input[0])).abs() < 250),
            );
            let value = if stage == 0 { 6000 } else { -5000 };
            outgoing
                .send(Frame::new(&[value; 160], 8000)?)
                .await
                .map_err(failure)?;
            let mut bytes = [0; 2049];
            let (size, source) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
            if source != local {
                return Err(MediaError::InvalidRoute);
            }
            let packet = RtpPacket::parse(&bytes[..size])?;
            observed.push(
                packet.payload().len() == 160
                    && packet.payload().iter().all(|actual| {
                        (i32::from(mulaw_decode(*actual)) - i32::from(value)).abs() < 250
                    }),
            );
        }
        Ok::<_, MediaError>(observed)
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    stop.send_replace(true);
    let stats = finish(task).await;
    let (observed, stats) = match (result, stats) {
        (Ok(observed), Ok(stats)) => (observed, stats),
        (Err(primary), Err(cleanup)) => {
            return Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(error),
    };
    assert_eq!(observed, [true; 4]);
    assert_eq!(stats.packets_received, 2);
    assert_eq!(stats.packets_sent, 2);
    assert_eq!(stats.dropped, malformed.len() as u64);
    let replacement =
        SocketPair::bind(local.ip(), local.port(), local.port() + 1).map_err(failure)?;
    assert_eq!(replacement.local_addr().map_err(failure)?, local);
    Ok(())
}

enum ReceiptFailure {
    Closed,
    Full,
}

#[tokio::test]
async fn reception_reports_cover_transmitted_sequences_across_gaps_and_rollover(
) -> Result<(), MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let Fixture { mut link, .. } = fixture(&peer)?;
    let ssrc = 22_u32;
    let mut report = [0_u8; 32];
    report[..8].copy_from_slice(&[0x81, 201, 0, 7, 0, 0, 0, 21]);
    report[8..12].copy_from_slice(&ssrc.to_be_bytes());
    for (sequence, accepted, rejected) in [
        (65532, 65532_u32, 65531_u32),
        (65534, 65534, 65535),
        (1, 65537, 65538),
        (3, 3, 131075),
    ] {
        let packet = RtpPacket::new(0, sequence, 0, ssrc, &[0; 160])?;
        super::record(
            &mut link.leg.sources,
            &packet,
            8000,
            0,
            tokio::time::Instant::now(),
        )?;
        report[16..20].copy_from_slice(&accepted.to_be_bytes());
        link.leg.received_control(&report)?;
        let latest = |link: &RtpLink| {
            link.leg
                .measurements()
                .peer
                .as_ref()
                .map(|peer| peer.extended_highest_sequence)
        };
        assert_eq!(latest(&link), Some(accepted));
        report[16..20].copy_from_slice(&rejected.to_be_bytes());
        link.leg.received_control(&report)?;
        assert_eq!(latest(&link), Some(accepted));
    }
    let (reports, _) = super::reports(&link.leg.sources)?;
    assert_eq!(reports[0].packets, 4);
    assert_eq!(reports[0].octets, 640);
    Ok(())
}

#[tokio::test]
async fn live_counts_follow_peer_datagrams_and_bind_reception_reports_to_the_sent_source(
) -> Result<(), MediaError> {
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let Fixture {
        local,
        control,
        link,
        received: _received,
        outgoing,
        stop,
        _direction,
    } = fixture(&peer)?;
    let mut observations = link.measurements.subscribe();
    assert_eq!(observations.borrow().local.rxcount, 0);
    assert_eq!(observations.borrow().local.txcount, 0);
    assert!(observations.borrow().peer.is_none());
    let (receipts, mut reports) = mpsc::channel(4);
    let task = tokio::spawn(
        link.serve(move |receipt| receipts.try_send(receipt).map_err(|_| MediaError::Capacity)),
    );
    let traffic = tokio::time::timeout(Duration::from_secs(3), async {
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [4; 10])?;
        let frame = Frame::new(&[750; 160], 8000)?;
        let incoming = sender.packet_at(frame, 20)?.encode()?;
        peer.data()
            .send_to(&incoming, local)
            .await
            .map_err(failure)?;
        observations
            .wait_for(|snapshot| snapshot.local.rxcount == 1)
            .await
            .map_err(failure)?;
        assert_eq!(observations.borrow().local.txcount, 0);
        outgoing.send(frame).await.map_err(failure)?;
        let mut buffer = [0; rusterisk_media::MAX_PACKET];
        let (size, _) = peer.data().recv_from(&mut buffer).await.map_err(failure)?;
        let sent = RtpPacket::parse(&buffer[..size])?;
        observations
            .wait_for(|snapshot| snapshot.local.txcount == 1)
            .await
            .map_err(failure)?;
        assert_eq!(observations.borrow().local.rxcount, 1);
        assert!(observations.borrow().peer.is_none());
        let mut report = [0_u8; 32];
        report[..8].copy_from_slice(&[0x81, 201, 0, 7, 0, 0, 0, 21]);
        report[8..12].copy_from_slice(&sent.ssrc().wrapping_add(1).to_be_bytes());
        report[13..16].copy_from_slice(&[0xff, 0xff, 0xff]);
        report[16..20].copy_from_slice(&u32::from(sent.sequence()).to_be_bytes());
        peer.control()
            .send_to(&report, control)
            .await
            .map_err(failure)?;
        assert!(matches!(
            reports.recv().await,
            Some(ControlReception::Accepted)
        ));
        assert!(observations.borrow().peer.is_none());
        report[8..12].copy_from_slice(&sent.ssrc().to_be_bytes());
        peer.control()
            .send_to(&report, control)
            .await
            .map_err(failure)?;
        assert!(matches!(
            reports.recv().await,
            Some(ControlReception::Accepted)
        ));
        let snapshot = observations.borrow().clone();
        let remote = snapshot.peer.as_ref().ok_or(MediaError::InvalidPacket)?;
        assert_eq!(remote.reporter_ssrc, 21);
        assert_eq!(remote.reported_ssrc, sent.ssrc());
        assert_eq!(remote.extended_highest_sequence, u32::from(sent.sequence()));
        assert_eq!(remote.cumulative_lost, -1);
        tokio::time::sleep(Duration::from_millis(45)).await;
        assert_eq!(observations.borrow().local, snapshot.local);
        Ok::<(), MediaError>(())
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    stop.send_replace(true);
    let cleanup = finish(task).await;
    match (traffic, cleanup) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(primary), Err(cleanup)) => {
            Err(failure(format!("{primary}; cleanup failed: {cleanup}")))
        }
        (Err(error), Ok(_)) | (Ok(()), Err(error)) => Err(error),
    }
}

#[tokio::test]
async fn receipt_failures_end_the_owned_worker_and_release_its_sockets() -> Result<(), MediaError> {
    for failure_kind in [ReceiptFailure::Closed, ReceiptFailure::Full] {
        let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
        let Fixture {
            local,
            control,
            link,
            received: _received,
            outgoing: _outgoing,
            stop,
            _direction,
        } = fixture(&peer)?;
        let (receipts, processed) = mpsc::channel(8);
        let _processed = match failure_kind {
            ReceiptFailure::Closed => {
                drop(processed);
                None
            }
            ReceiptFailure::Full => {
                for _ in 0..8 {
                    receipts
                        .try_send(ControlReception::Rejected)
                        .map_err(failure)?;
                }
                Some(processed)
            }
        };
        let task =
            tokio::spawn(link.serve(move |receipt| {
                receipts.try_send(receipt).map_err(|_| MediaError::Capacity)
            }));
        let packet = [0x80, 201, 0, 2, 0, 0, 0, 7];
        let operation = peer
            .control()
            .send_to(&packet, control)
            .await
            .map_err(failure)
            .and_then(|size| {
                if size == packet.len() {
                    Ok(())
                } else {
                    Err(MediaError::InvalidPacket)
                }
            });
        if operation.is_err() {
            stop.send_replace(true);
        }
        let outcome = finish(task).await;
        if let Err(primary) = operation {
            return match outcome {
                Ok(_) => Err(primary),
                Err(cleanup) => Err(failure(format!("{primary}; cleanup failed: {cleanup}"))),
            };
        }
        let replacement =
            SocketPair::bind(local.ip(), local.port(), local.port() + 1).map_err(failure)?;
        assert_eq!(replacement.local_addr().map_err(failure)?, local);
        assert_eq!(outcome.err(), Some(MediaError::Capacity));
    }
    Ok(())
}

async fn delivered_sequence(
    outgoing: &mpsc::Sender<Frame>,
    peer: &SocketPair,
    sample: i16,
) -> Result<u16, MediaError> {
    outgoing
        .send(Frame::new(&[sample; 160], 8000)?)
        .await
        .map_err(failure)?;
    let mut packet = [0; 256];
    let (size, _) =
        tokio::time::timeout(Duration::from_secs(2), peer.data().recv_from(&mut packet))
            .await
            .map_err(failure)?
            .map_err(failure)?;
    Ok(RtpPacket::parse(&packet[..size])?.sequence())
}

struct DroppedReports(watch::Sender<u32>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DroppedReports {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().name() == "control_report_dropped" {
            self.0.send_modify(|count| *count = count.saturating_add(1));
        }
    }
}

#[tokio::test]
async fn failed_control_reports_leave_the_audio_stream_running() -> Result<(), MediaError> {
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let mut phone = fixture(&peer)?;
    let other_family = tokio::net::UdpSocket::bind((std::net::Ipv6Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    assert!(other_family
        .send_to(&[0], peer.control_addr().map_err(failure)?)
        .await
        .is_err());
    phone.link.leg.control = other_family.into_std().map_err(failure)?;
    let (observer, mut dropped) = watch::channel(0);
    let subscriber = tracing_subscriber::registry().with(DroppedReports(observer));
    let task = tokio::spawn(phone.link.run().with_subscriber(subscriber));
    let first = delivered_sequence(&phone.outgoing, &peer, 750).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(3200)).await;
    tokio::time::resume();
    tokio::time::timeout(
        Duration::from_secs(2),
        dropped.wait_for(|count| *count >= 1),
    )
    .await
    .map_err(failure)?
    .map_err(|_| failure("the worker ended before its periodic report"))?;
    let second = delivered_sequence(&phone.outgoing, &peer, 1500).await?;
    assert_eq!(second, first.wrapping_add(1));
    phone.stop.send_replace(true);
    let stats = finish(task).await?;
    assert_eq!(stats.packets_sent, 2);
    assert!(
        stats.dropped >= 2,
        "the periodic report and the closing report both count as dropped"
    );
    Ok(())
}
