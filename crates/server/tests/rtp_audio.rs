use rusterisk_media::{mulaw_decode, AudioFormat, Frame, MediaError, RtpPacket, RtpSender};
use rusterisk_server::rtp::{RtpChannels, RtpLink, RtpSession, SocketPair};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

#[tokio::test]
async fn real_datagrams_carry_known_audio_in_both_directions_and_release_ports(
) -> Result<(), MediaError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (incoming, mut received) = mpsc::channel(3);
    let (outgoing, sent) = mpsc::channel(3);
    let (stop, stopped) = watch::channel(false);
    let link = RtpLink::new(
        sockets,
        RtpSession {
            remote: peer.local_addr().map_err(failure)?,
            format: AudioFormat::Pcmu,
            payload_type: 0,
        },
        RtpChannels {
            incoming,
            outgoing: sent,
            stop: stopped,
        },
    )?;
    let task = tokio::spawn(link.run());
    let result = async {
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [7; 10])?;
        let attacker = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(failure)?;
        let noise = sender.packet(Frame::new(&[-20000; 160], 8000)?)?.encode()?;
        attacker.send_to(&noise, local).await.map_err(failure)?;
        let input =
            std::array::from_fn::<_, 160, _>(|index| if index % 20 < 10 { 4000 } else { -4000 });
        let bytes = sender.packet(Frame::new(&input, 8000)?)?.encode()?;
        peer.data().send_to(&bytes, local).await.map_err(failure)?;
        let audio = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .map_err(failure)?
            .ok_or(MediaError::InvalidFrame)?;
        assert_eq!(audio.samples().len(), input.len());
        assert!(audio
            .samples()
            .iter()
            .zip(input)
            .all(|(actual, expected)| (i32::from(*actual) - i32::from(expected)).abs() < 150));
        let output =
            std::array::from_fn::<_, 160, _>(|index| if index % 16 < 8 { 6000 } else { -6000 });
        outgoing
            .send(Frame::new(&output, 8000)?)
            .await
            .map_err(failure)?;
        let mut buffer = [0; 1501];
        let (size, source) =
            tokio::time::timeout(Duration::from_secs(2), peer.data().recv_from(&mut buffer))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        assert_eq!(source, local);
        let packet = RtpPacket::parse(&buffer[..size])?;
        assert!(packet
            .payload()
            .iter()
            .map(|byte| mulaw_decode(*byte))
            .zip(output)
            .all(|(actual, expected)| (i32::from(actual) - i32::from(expected)).abs() < 250));
        Ok(())
    }
    .await;
    stop.send_replace(true);
    let stats = task.await.map_err(failure)??;
    result?;
    assert_eq!(stats.packets_received, 1);
    assert_eq!(stats.packets_sent, 1);
    let replacement = SocketPair::bind(ip, local.port(), local.port() + 1).map_err(failure)?;
    assert_eq!(replacement.local_addr().map_err(failure)?, local);
    Ok(())
}
