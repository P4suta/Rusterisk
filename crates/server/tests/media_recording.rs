use rusterisk_api::ChannelId;
use rusterisk_core::{MediaTaskKind, MediaTasks};
use rusterisk_media::{AudioFormat, Frame, MediaError, RtpSender};
use rusterisk_server::media::{MediaHub, RecordingEnd, RecordingTarget};
use rusterisk_server::rtp::{RtpSession, SocketPair};
use rusterisk_server::storage::StorageWorker;
use rusterisk_server::CallKey;
use rusterisk_storage::WaveReader;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

fn id(value: &str) -> Result<ChannelId, MediaError> {
    ChannelId::try_from(value.to_owned()).map_err(failure)
}

#[tokio::test]
async fn channel_and_bridge_recordings_contain_their_actual_received_audio(
) -> Result<(), MediaError> {
    let directory = tempfile::tempdir().map_err(failure)?;
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
        let alice_key = CallKey {
            id: id("alice")?,
            generation: 1,
        };
        let bob_key = CallKey {
            id: id("bob")?,
            generation: 2,
        };
        let mut addresses = Vec::new();
        for (key, peer) in [(alice_key.clone(), &alice), (bob_key.clone(), &bob)] {
            let sockets = SocketPair::bind(ip, 0, 0).map_err(failure)?;
            addresses.push(sockets.local_addr().map_err(failure)?);
            media
                .attach(
                    key,
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
        media.bridge(10, [alice_key.clone(), bob_key].into_iter().collect())?;
        let mut channel_tasks = MediaTasks::new(1);
        let channel = channel_tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let mut bridge_tasks = MediaTasks::new(10);
        let bridge = bridge_tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let channel_done = media
            .record(
                RecordingTarget::Channel(alice_key),
                channel,
                storage
                    .record(id("channel")?, 8000)
                    .await
                    .map_err(failure)?,
            )
            .await?;
        let bridge_done = media
            .record(
                RecordingTarget::Bridge(10),
                bridge,
                storage.record(id("bridge")?, 8000).await.map_err(failure)?,
            )
            .await?;
        let mut alice_sender = RtpSender::new(AudioFormat::Pcmu, 0, [1; 10])?;
        let mut bob_sender = RtpSender::new(AudioFormat::Pcmu, 0, [2; 10])?;
        let mut period = tokio::time::interval(Duration::from_millis(20));
        period.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..20 {
            period.tick().await;
            for (peer, sender, destination, sample) in [
                (&alice, &mut alice_sender, addresses[0], 1200),
                (&bob, &mut bob_sender, addresses[1], -3400),
            ] {
                let packet = sender.packet(Frame::new(&[sample; 160], 8000)?)?.encode()?;
                peer.data()
                    .send_to(&packet, destination)
                    .await
                    .map_err(failure)?;
            }
        }
        media.stop_recording(channel, RecordingEnd::Finish)?;
        media.stop_recording(bridge, RecordingEnd::Finish)?;
        for completed in [channel_done, bridge_done] {
            tokio::time::timeout(Duration::from_secs(2), completed)
                .await
                .map_err(failure)?
                .map_err(failure)?
                .map_err(failure)?;
        }
        for (name, expected) in [("channel", 1200i32), ("bridge", -2200i32)] {
            let mut reader =
                WaveReader::open(&directory.path().join(format!("recordings/{name}.wav")))
                    .map_err(failure)?;
            let mut consecutive = 0;
            let mut longest = 0;
            while let Some(frame) = reader.read_frame().map_err(failure)? {
                if frame
                    .samples()
                    .iter()
                    .all(|sample| (i32::from(*sample) - expected).abs() < 300)
                {
                    consecutive += 1;
                    longest = longest.max(consecutive);
                } else {
                    consecutive = 0;
                }
            }
            assert!(longest >= 6, "{name}: {longest} matching frames");
        }
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    worker.close().await.map_err(failure)?;
    result
}
