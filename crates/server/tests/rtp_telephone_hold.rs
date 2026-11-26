use rusterisk_core::{DtmfDigit, DtmfTone, PeerMediaDirection};
use rusterisk_media::{AudioFormat, MediaError, RtpPacket, TelephoneEvent, TelephoneFormat};
use rusterisk_test_support::Wait;
#[path = "rtp_leg/driver.rs"]
mod driver;

use driver::RtpLink;
use rusterisk_media::MAX_PACKET;
use rusterisk_server::rtp::{
    ControlReception, LinkStats, Measurements, RtpChannels, RtpLeg, RtpSession, SocketPair,
};
use std::net::{IpAddr, Ipv4Addr};
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn packet(
    digit: DtmfDigit,
    timestamp: u32,
    duration: u16,
    ended: bool,
    sequence: u16,
) -> Result<RtpPacket, MediaError> {
    RtpPacket::new(
        101,
        sequence,
        timestamp,
        7,
        &TelephoneEvent::new(digit, ended, 10, duration)?.encode(),
    )
}

#[tokio::test]
async fn held_events_do_not_complete_after_rtp_reception_resumes() -> Result<(), MediaError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (incoming, _audio) = mpsc::channel(3);
    let (digits, mut received) = mpsc::channel(16);
    let (_outgoing, output) = mpsc::channel(3);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (direction, directed) = watch::channel(PeerMediaDirection::ReceiveOnly);
    let link = RtpLink::new(
        sockets,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
            telephone: Some(TelephoneFormat::new(101, 8000)?),
        },
        RtpChannels {
            incoming,
            digits,
            outgoing: output,
            commands,
            completed,
            stop: stopped,
            direction: directed,
        },
    )?;
    let mut task = tokio::spawn(link.run());
    let result = Wait::Ready
        .bounded(async {
            peer.data()
                .send_to(
                    &packet(DtmfDigit::Hash, 1000, 160, false, 1)?.encode()?,
                    local,
                )
                .await
                .map_err(failure)?;
            if Wait::Quiet.bounded(received.recv()).await.is_ok() {
                return Err(failure("A held event was delivered"));
            }
            direction.send_replace(PeerMediaDirection::SendReceive);
            peer.data()
                .send_to(
                    &packet(DtmfDigit::Hash, 1000, 800, true, 2)?.encode()?,
                    local,
                )
                .await
                .map_err(failure)?;
            if Wait::Quiet.bounded(received.recv()).await.is_ok() {
                return Err(failure(
                    "A held event completed after RTP reception resumed",
                ));
            }
            peer.data()
                .send_to(
                    &packet(DtmfDigit::Two, 1800, 320, true, 3)?.encode()?,
                    local,
                )
                .await
                .map_err(failure)?;
            let tone = received.recv().await.ok_or(MediaError::UnknownStream)?;
            if tone
                != (DtmfTone {
                    digit: DtmfDigit::Two,
                    duration_ms: 40,
                })
            {
                return Err(failure(
                    "A distinct event was not received after RTP reception resumed",
                ));
            }
            Ok::<(), MediaError>(())
        })
        .await
        .map_err(failure);
    stop.send_replace(true);
    let cleanup = match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?.map(|_| ()),
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    };
    result??;
    cleanup
}
