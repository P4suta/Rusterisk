use rusterisk_api::ChannelId;
use rusterisk_core::{MediaTaskKind, MediaTasks};
use rusterisk_media::{mulaw_decode, AudioFormat, AudioTarget, Frame, MediaError, RtpPacket};
use rusterisk_server::media::{MediaHub, RecordingEnd, RecordingTarget};
use rusterisk_server::rtp::{RtpSession, SocketPair};
use rusterisk_server::storage::{PlaybackMedia, StorageWorker};
use rusterisk_server::CallKey;
use rusterisk_storage::{Recorder, WaveReader};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}
fn id(value: &str) -> Result<ChannelId, MediaError> {
    ChannelId::try_from(value.to_owned()).map_err(failure)
}

async fn received(peer: &SocketPair) -> Result<(), MediaError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = [0; 1501];
        let mut consecutive = 0;
        for _ in 0..100 {
            let (size, _) = peer.data().recv_from(&mut bytes).await.map_err(failure)?;
            let packet = RtpPacket::parse(&bytes[..size])?;
            if packet.payload_type() == 0
                && packet
                    .payload()
                    .iter()
                    .all(|sample| (i32::from(mulaw_decode(*sample)) - 4000).abs() < 300)
            {
                consecutive += 1;
                if consecutive == 6 {
                    return Ok(());
                }
            } else {
                consecutive = 0;
            }
        }
        Err(MediaError::InvalidFrame)
    })
    .await
    .map_err(failure)?
}

#[tokio::test]
async fn bridge_playback_reaches_every_peer_and_is_included_in_its_recording(
) -> Result<(), MediaError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let mut fixture = Recorder::create(directory.path(), "source", 8000).map_err(failure)?;
    for _ in 0..20 {
        fixture
            .write(Frame::new(&[4000; 160], 8000)?)
            .map_err(failure)?;
    }
    fixture.finish().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let (media, hub, _notices) = MediaHub::new(2);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let alice = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let bob = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let mut keys = Vec::new();
        for (name, generation, peer) in [("alice", 1, &alice), ("bob", 2, &bob)] {
            let key = CallKey {
                id: id(name)?,
                generation,
            };
            keys.push(key.clone());
            media
                .attach(
                    key,
                    SocketPair::bind(ip, 0, 0).map_err(failure)?,
                    RtpSession {
                        remote: peer.local_addr().map_err(failure)?,
                        format: AudioFormat::Pcmu,
                        payload_type: 0,
                        telephone: None,
                    },
                )
                .await?;
        }
        media.bridge(10, keys)?;
        let mut tasks = MediaTasks::new(10);
        let record = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let recorded = media
            .record(
                RecordingTarget::Bridge(10),
                record,
                storage
                    .record(id("received")?, 8000)
                    .await
                    .map_err(failure)?,
            )
            .await?;
        let playback = tasks
            .reserve(MediaTaskKind::Playback)
            .map_err(|_| MediaError::Capacity)?;
        let completed = media
            .play(
                AudioTarget::Bridge(10),
                playback,
                storage
                    .play(PlaybackMedia::Recording(id("source")?))
                    .await
                    .map_err(failure)?,
            )
            .await?;
        tokio::try_join!(received(&alice), received(&bob))?;
        tokio::time::timeout(Duration::from_secs(2), completed)
            .await
            .map_err(failure)?
            .map_err(failure)??;
        media.stop_recording(record, RecordingEnd::Finish)?;
        tokio::time::timeout(Duration::from_secs(2), recorded)
            .await
            .map_err(failure)?
            .map_err(failure)?
            .map_err(failure)?;
        let mut reader =
            WaveReader::open(&directory.path().join("recordings/received.wav")).map_err(failure)?;
        let mut matching = 0;
        while let Some(frame) = reader.read_frame().map_err(failure)? {
            if frame.samples() == [4000; 160] {
                matching += 1;
            }
        }
        assert_eq!(matching, 20);
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    worker.close().await.map_err(failure)?;
    result
}
