use super::{ControlReception, LinkStats, RtpChannels, RtpLink, RtpSession, SocketPair};
use rusterisk_core::PeerMediaDirection;
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
