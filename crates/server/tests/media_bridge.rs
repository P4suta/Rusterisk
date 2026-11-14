use rusterisk_api::ChannelId;
use rusterisk_core::{DtmfDigit, DtmfTone};
use rusterisk_media::{
    mulaw_decode, AudioFormat, Frame, MediaError, RtpPacket, RtpSender, TelephoneEvent,
    TelephoneFormat,
};
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
                        telephone: None,
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

#[tokio::test]
async fn telephone_datagrams_emit_one_digit_without_claiming_an_audio_connection(
) -> Result<(), MediaError> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let peer = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let (media, hub, mut notices) = MediaHub::new(1);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        media
            .attach(
                key("alice")?,
                sockets,
                RtpSession {
                    remote: peer.local_addr().map_err(failure)?,
                    format: AudioFormat::Pcmu,
                    payload_type: 0,
                    telephone: Some(TelephoneFormat::new(101, 8000)?),
                },
            )
            .await?;
        for sequence in 1..=3 {
            let event = TelephoneEvent::new(DtmfDigit::Hash, true, 10, 960)?;
            let bytes = RtpPacket::new(101, sequence, 1000, 7, &event.encode())?.encode()?;
            peer.data().send_to(&bytes, local).await.map_err(failure)?;
        }
        let notice = tokio::time::timeout(Duration::from_secs(2), notices.recv())
            .await
            .map_err(failure)?;
        assert!(matches!(notice, Some(MediaNotice::Dtmf { key: call, tone })
            if call == key("alice")? && tone.digit == DtmfDigit::Hash && tone.duration_ms == 120));
        peer.data()
            .send_to(
                &RtpPacket::new(0, 4, 1960, 7, &[0xff; 160])?.encode()?,
                local,
            )
            .await
            .map_err(failure)?;
        let notice = tokio::time::timeout(Duration::from_secs(2), notices.recv())
            .await
            .map_err(failure)?;
        assert!(matches!(notice, Some(MediaNotice::Connected { .. })));
        assert!(notices.try_recv().is_err());
        media
            .send_tone(
                key("alice")?,
                DtmfTone {
                    digit: DtmfDigit::One,
                    duration_ms: 60,
                },
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = [0; 1501];
            let mut ends = 0;
            for _ in 0..100 {
                let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
                let packet = RtpPacket::parse(&bytes[..size])?;
                if packet.payload_type() == 101 {
                    let tone = TelephoneEvent::parse(packet.payload())?;
                    assert_eq!(tone.digit(), DtmfDigit::One);
                    if tone.ended() {
                        ends += 1;
                    }
                    if ends == 3 {
                        return Ok(());
                    }
                }
            }
            Err(MediaError::InvalidPacket)
        })
        .await
        .map_err(failure)??;
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    result
}
