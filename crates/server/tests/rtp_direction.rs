use rusterisk_core::PeerMediaDirection;
use rusterisk_media::{AudioFormat, ControlKind, Frame, MediaError, RtpPacket, RtpSender};
use rusterisk_server::rtp::{RtpChannels, RtpLink, RtpSession, SocketPair};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn negotiated_direction_controls_received_and_sent_audio() -> Result<(), MediaError> {
    for direction in [
        PeerMediaDirection::SendReceive,
        PeerMediaDirection::SendOnly,
        PeerMediaDirection::ReceiveOnly,
        PeerMediaDirection::Inactive,
    ] {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let local = sockets.local_addr().map_err(failure)?;
        let (incoming, mut received) = mpsc::channel(3);
        let (digits, _digits) = mpsc::channel(16);
        let (outgoing, sent) = mpsc::channel(3);
        let (_commands, commands) = mpsc::channel(8);
        let (completed, _completed) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (_direction, directed) = watch::channel(direction);
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
                outgoing: sent,
                commands,
                completed,
                stop: stopped,
                direction: directed,
            },
        )?;
        let task = tokio::spawn(link.run());
        let result = async {
            let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [7; 10])?;
            let packet = sender.packet(Frame::new(&[4000; 160], 8000)?)?.encode()?;
            peer.data().send_to(&packet, local).await.map_err(failure)?;
            outgoing
                .send(Frame::new(&[-7000; 160], 8000)?)
                .await
                .map_err(failure)?;
            let audio = tokio::time::timeout(Duration::from_millis(150), received.recv()).await;
            assert_eq!(audio.is_ok(), direction.can_send(), "{direction:?}");
            if let Ok(audio) = audio {
                let audio = audio.ok_or(MediaError::InvalidFrame)?;
                assert!(audio
                    .samples()
                    .iter()
                    .all(|sample| (i32::from(*sample) - 4000).abs() < 150));
            }
            let mut bytes = [0; 1501];
            let packet = tokio::time::timeout(
                Duration::from_millis(150),
                peer.data().recv_from(&mut bytes),
            )
            .await;
            assert_eq!(packet.is_ok(), direction.can_receive(), "{direction:?}");
            if let Ok(packet) = packet {
                let (size, _) = packet.map_err(failure)?;
                assert_eq!(RtpPacket::parse(&bytes[..size])?.payload_type(), 0);
            }
            Ok(())
        }
        .await;
        stop.send_replace(true);
        let stats = task.await.map_err(failure)??;
        result?;
        assert_eq!(stats.packets_received, u64::from(direction.can_send()));
        assert_eq!(stats.packets_sent, u64::from(direction.can_receive()));
    }
    Ok(())
}

#[tokio::test]
async fn inactive_update_still_sends_control_goodbye() -> Result<(), MediaError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let (incoming, _received) = mpsc::channel(3);
    let (digits, _digits) = mpsc::channel(16);
    let (outgoing, sent) = mpsc::channel(3);
    let (_commands, commands) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
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
            outgoing: sent,
            commands,
            completed,
            stop: stopped,
            direction: directed,
        },
    )?;
    let task = tokio::spawn(link.run());
    let result = async {
        outgoing
            .send(Frame::new(&[4000; 160], 8000)?)
            .await
            .map_err(failure)?;
        let mut bytes = [0; 1501];
        tokio::time::timeout(Duration::from_secs(2), peer.data().recv_from(&mut bytes))
            .await
            .map_err(failure)?
            .map_err(failure)?;
        direction.send_replace(PeerMediaDirection::Inactive);
        outgoing
            .send(Frame::new(&[-7000; 160], 8000)?)
            .await
            .map_err(failure)?;
        assert!(tokio::time::timeout(
            Duration::from_millis(150),
            peer.data().recv_from(&mut bytes)
        )
        .await
        .is_err());
        Ok(())
    }
    .await;
    stop.send_replace(true);
    let stats = task.await.map_err(failure)??;
    result?;
    assert_eq!(stats.packets_sent, 1);
    let mut bytes = [0; 1501];
    let (size, _) =
        tokio::time::timeout(Duration::from_secs(2), peer.control().recv_from(&mut bytes))
            .await
            .map_err(failure)?
            .map_err(failure)?;
    assert_eq!(
        rusterisk_media::inspect_rtcp(&bytes[..size])?.last(),
        Some(&ControlKind::Goodbye)
    );
    Ok(())
}
