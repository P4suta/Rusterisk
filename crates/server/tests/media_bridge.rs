use rusterisk_api::ChannelId;
use rusterisk_media::{mulaw_decode, AudioFormat, Frame, MediaError, RtpPacket, RtpSender};
use rusterisk_server::media::{MediaHub, MediaNotice};
use rusterisk_server::rtp::{RtpSession, SocketPair};
use rusterisk_server::CallKey;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}
fn key(id: &str) -> Result<CallKey, MediaError> {
    Ok(CallKey {
        id: ChannelId::try_from(id.to_owned()).map_err(failure)?,
        generation: 1,
    })
}

async fn receives(peer: &SocketPair, expected: i32) -> Result<(), MediaError> {
    let mut bytes = [0; 1501];
    tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..100 {
            let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
            let packet = RtpPacket::parse(&bytes[..size])?;
            if packet.payload().len() == 160
                && packet
                    .payload()
                    .iter()
                    .all(|byte| (i32::from(mulaw_decode(*byte)) - expected).abs() < 300)
            {
                return Ok(());
            }
        }
        Err(MediaError::InvalidFrame)
    })
    .await
    .map_err(failure)?
}

#[tokio::test]
async fn owned_workers_bridge_real_received_audio_and_stop_together() -> Result<(), MediaError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let alice = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let bob = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let alice_server = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let bob_server = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let alice_address = alice_server.local_addr().map_err(failure)?;
    let bob_address = bob_server.local_addr().map_err(failure)?;
    let (media, hub, mut notices) = MediaHub::new(4);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        for (id, sockets, peer) in [("alice", alice_server, &alice), ("bob", bob_server, &bob)] {
            media
                .attach(
                    key(id)?,
                    sockets,
                    RtpSession {
                        remote: peer.local_addr().map_err(failure)?,
                        format: AudioFormat::Pcmu,
                        payload_type: 0,
                    },
                )
                .await?;
        }
        media.bridge(1, [key("alice")?, key("bob")?].into_iter().collect())?;
        let mut sender = RtpSender::new(AudioFormat::Pcmu, 0, [8; 10])?;
        for (peer, destination, value) in
            [(&alice, alice_address, 4000), (&bob, bob_address, -7000)]
        {
            let bytes = sender.packet(Frame::new(&[value; 160], 8000)?)?.encode()?;
            peer.data()
                .send_to(&bytes, destination)
                .await
                .map_err(failure)?;
        }
        tokio::try_join!(receives(&alice, -7000), receives(&bob, 4000))?;
        for _ in 0..2 {
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(2), notices.recv())
                    .await
                    .map_err(failure)?,
                Some(MediaNotice::Connected { .. })
            ));
        }
        media.release(key("alice")?)?;
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    result?;
    for address in [alice_address, bob_address] {
        let sockets = SocketPair::bind(ip, address.port(), address.port() + 1).map_err(failure)?;
        assert_eq!(sockets.local_addr().map_err(failure)?, address);
    }
    Ok(())
}
