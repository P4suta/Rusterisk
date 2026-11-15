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
async fn a_deleted_bridge_rejects_late_recording_files_without_stopping_its_replacement(
) -> Result<(), MediaError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let (media, hub, _notices) = MediaHub::new(1);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        let mut old_tasks = MediaTasks::new(10);
        let old = old_tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let old_done = media
            .reserve_recording(RecordingTarget::Bridge(10), old)
            .await?;
        media.release_bridge(10)?;
        let mut current_tasks = MediaTasks::new(20);
        let current = current_tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let current_done = media
            .reserve_recording(RecordingTarget::Bridge(20), current)
            .await?;
        let late = storage.record(id("late")?, 8000).await.map_err(failure)?;
        assert!(matches!(
            media.attach_recording(old, late).await,
            Err(MediaError::UnknownStream)
        ));
        let sink = storage
            .record(id("current")?, 8000)
            .await
            .map_err(failure)?;
        media.attach_recording(current, sink).await?;
        media.release_bridge(10)?;
        media.stop_recording(current, RecordingEnd::Finish)?;
        for done in [old_done, current_done] {
            tokio::time::timeout(Duration::from_secs(2), done)
                .await
                .map_err(failure)?
                .map_err(failure)?
                .map_err(failure)?;
        }
        let replacement = storage.record(id("late")?, 8000).await.map_err(failure)?;
        replacement.cancel().await.map_err(failure)?;
        assert_eq!(
            storage
                .recordings()
                .await
                .map_err(failure)?
                .iter()
                .map(|recording| recording.name.as_str())
                .collect::<Vec<_>>(),
            ["current"]
        );
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    worker.close().await.map_err(failure)?;
    result
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
        media.release_bridge(10)?;
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

#[tokio::test]
async fn cancellation_removes_audio_and_shutdown_finishes_owned_recordings(
) -> Result<(), MediaError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let (media, hub, _notices) = MediaHub::new(1);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        let mut tasks = MediaTasks::new(10);
        let cancelled = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let completion = media
            .record(
                RecordingTarget::Bridge(10),
                cancelled,
                storage
                    .record(id("cancelled")?, 8000)
                    .await
                    .map_err(failure)?,
            )
            .await?;
        media.stop_recording(cancelled, RecordingEnd::Cancel)?;
        tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .map_err(failure)?
            .map_err(failure)?
            .map_err(failure)?;
        assert!(!directory
            .path()
            .join("recordings/cancelled.partial")
            .exists());
        assert!(!directory.path().join("recordings/cancelled.wav").exists());
        assert!(tasks.finish(cancelled));
        let final_token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let final_completion = media
            .record(
                RecordingTarget::Bridge(10),
                final_token,
                storage
                    .record(id("shutdown")?, 8000)
                    .await
                    .map_err(failure)?,
            )
            .await?;
        Ok(final_completion)
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    let completion = result?;
    completion.await.map_err(failure)?.map_err(failure)?;
    assert!(directory.path().join("recordings/shutdown.wav").exists());
    worker.close().await.map_err(failure)?;
    Ok(())
}

#[tokio::test]
async fn failed_publication_is_reported_and_its_partial_audio_recovers_after_restart(
) -> Result<(), MediaError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    let (media, hub, _notices) = MediaHub::new(1);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(hub.run(stopped));
    let result = async {
        let mut tasks = MediaTasks::new(10);
        let token = tasks
            .reserve(MediaTaskKind::Recording)
            .map_err(|_| MediaError::Capacity)?;
        let completion = media
            .record(
                RecordingTarget::Bridge(10),
                token,
                storage
                    .record(id("recover")?, 8000)
                    .await
                    .map_err(failure)?,
            )
            .await?;
        std::fs::create_dir(directory.path().join("recordings/recover.wav")).map_err(failure)?;
        media.stop_recording(token, RecordingEnd::Finish)?;
        let outcome = tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .map_err(failure)?
            .map_err(failure)?;
        assert!(matches!(
            outcome,
            Err(rusterisk_server::storage::WorkerError::Storage(
                rusterisk_storage::StorageError::Io(_)
            ))
        ));
        Ok(())
    }
    .await;
    stop.send_replace(true);
    task.await.map_err(failure)??;
    worker.close().await.map_err(failure)?;
    result?;
    std::fs::remove_dir(directory.path().join("recordings/recover.wav")).map_err(failure)?;
    let (storage, worker) = StorageWorker::start(directory.path().to_owned())
        .await
        .map_err(failure)?;
    assert_eq!(
        storage.recordings().await.map_err(failure)?[0].name,
        "recover"
    );
    assert!(!directory.path().join("recordings/recover.partial").exists());
    worker.close().await.map_err(failure)?;
    Ok(())
}
