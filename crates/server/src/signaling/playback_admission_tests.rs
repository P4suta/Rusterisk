use super::{cleanup::combine, failure, Active, DigitDelivery, Signaling};
use crate::media::MediaHub;
use crate::rtp::{RtpSession, SocketPair};
use crate::storage::StorageWorker;
use crate::{
    Action, CallKey, ControlError, Controller, Credentials, Effect, IncomingCall, IncomingTarget,
    MediaOwner, Milestone, Signal,
};
use rusterisk_api::{ChannelId, ChannelState, Event, EventKind, MediaUri, PlaybackState};
use rusterisk_config::Config;
use rusterisk_core::{DtmfDigit, MediaTaskKind, MediaTasks, PeerMediaDirection};
use rusterisk_media::{AudioFormat, Frame};
use rusterisk_storage::Recorder;
use std::{net::Ipv4Addr, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Readiness {
    Completed,
    Live,
}

#[tokio::test(flavor = "current_thread")]
async fn completed_playbacks_are_reaped_before_music_capacity_is_checked(
) -> Result<(), ControlError> {
    fixture(Readiness::Completed).await
}

#[tokio::test(flavor = "current_thread")]
async fn live_playbacks_keep_the_actor_capacity_limit_and_release_failed_music_admission(
) -> Result<(), ControlError> {
    fixture(Readiness::Live).await
}

fn id(name: &str) -> Result<ChannelId, ControlError> {
    Ok(ChannelId::try_from(name.to_owned())?)
}

fn credentials() -> Result<Credentials, ControlError> {
    Credentials::new("app".into(), "test-password".into())
}

async fn joined<E: std::fmt::Display>(
    mut task: JoinHandle<Result<(), E>>,
) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            let result = Err(failure(primary));
            match task.await {
                Err(error) if error.is_cancelled() => result,
                Err(error) => combine(result, Err(failure(error))),
                Ok(Err(error)) => combine(result, Err(failure(error))),
                Ok(Ok(())) => result,
            }
        }
    }
}

async fn fixture(readiness: Readiness) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = async {
        let config = Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:0\n",
            "",
            "",
        )
        .and_then(|config| {
            config.with_music_on_hold_configuration(
                "[default]\nmode=files\ndirectory=music\nsort=alpha\n",
            )
        })
        .map_err(failure)?;
        let mut recorder =
            Recorder::create(directory.path(), "admission-music", 8000).map_err(failure)?;
        recorder
            .write(Frame::new(&[1200; 160], 8000).map_err(failure)?)
            .map_err(failure)?;
        let wave = recorder.finish().map_err(failure)?;
        std::fs::create_dir(directory.path().join("music")).map_err(failure)?;
        std::fs::rename(wave, directory.path().join("music/known.wav")).map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let result = exercise(config, storage, readiness).await;
        combine(result, worker.close().await.map_err(failure))
    }
    .await;
    combine(result, directory.close().map_err(failure))
}

async fn answered(events: &mut mpsc::Receiver<Event>, key: &CallKey) -> Result<(), ControlError> {
    for _ in 0..64 {
        let event = events.recv().await.ok_or(ControlError::Capacity)?;
        if let EventKind::ChannelStateChange { channel } = event.kind {
            if channel.id == key.id && channel.state == ChannelState::Up {
                return Ok(());
            }
        }
    }
    Err(ControlError::Capacity)
}

async fn completion_marker(
    controller: &Controller,
    events: &mut mpsc::Receiver<Event>,
    key: &CallKey,
    playback: &ChannelId,
) -> Result<usize, ControlError> {
    controller
        .signal(Signal::DtmfReceived {
            key: key.clone(),
            digit: DtmfDigit::Five,
            duration_ms: 40,
        })
        .await?;
    let mut finished = 0;
    for _ in 0..64 {
        let event = events.recv().await.ok_or(ControlError::Capacity)?;
        if let EventKind::PlaybackFinished { playback: actual } = &event.kind {
            if &actual.id == playback {
                if actual.state != PlaybackState::Done {
                    return Err(ControlError::Invalid("Completed playback reported failure"));
                }
                finished += 1;
            }
        }
        if let EventKind::ChannelDtmfReceived {
            digit,
            channel,
            duration_ms,
        } = event.kind
        {
            if channel.id == key.id && digit == "5" && duration_ms == 40 {
                return Ok(finished);
            }
        }
    }
    Err(ControlError::Capacity)
}

async fn serve_until_completion(
    signaling: &mut Signaling,
    events: &mut mpsc::Receiver<Event>,
    playback: &ChannelId,
) -> Result<(), ControlError> {
    let (stop, mut stopped) = watch::channel(false);
    let mut serving = std::pin::pin!(signaling.serve(&mut stopped, std::time::Instant::now()));
    let completed = async {
        for _ in 0..64 {
            let event = events.recv().await.ok_or(ControlError::Capacity)?;
            if let EventKind::PlaybackFinished { playback: actual } = event.kind {
                if &actual.id == playback {
                    if actual.state != PlaybackState::Done {
                        return Err(ControlError::Invalid("Completed playback reported failure"));
                    }
                    return Ok(());
                }
            }
        }
        Err(ControlError::Capacity)
    };
    let result = tokio::select! {
        result = completed => result,
        result = &mut serving => {
            return combine(Err(ControlError::Invalid("Actor stopped before playback completion")), result);
        }
    };
    stop.send_replace(true);
    combine(result, serving.await)
}

async fn fill_jobs(
    signaling: &mut Signaling,
    readiness: Readiness,
    sentinel: MediaOwner,
    sentinel_token: rusterisk_core::MediaTaskToken,
    release: &watch::Sender<bool>,
) -> Result<Vec<AbortHandle>, ControlError> {
    let (entered, mut entries) = mpsc::channel(64);
    let mut handles = Vec::with_capacity(64);
    for index in 0..64 {
        let (owner, token) = if index == 0 {
            (sentinel.clone(), sentinel_token)
        } else {
            let generation = 1000 + index;
            let mut tasks = MediaTasks::new(generation);
            let token = tasks.reserve(MediaTaskKind::Playback)?;
            tasks.start(token)?;
            (
                MediaOwner::Channel(CallKey {
                    id: id(&format!("retired-{index}"))?,
                    generation,
                }),
                token,
            )
        };
        let entered = entered.clone();
        let mut released = release.subscribe();
        handles.push(signaling.playbacks.spawn(async move {
            let result = async {
                entered.try_send(()).map_err(failure)?;
                if readiness == Readiness::Live {
                    released
                        .wait_for(|release| *release)
                        .await
                        .map_err(failure)?;
                }
                Ok(())
            }
            .await;
            (owner, token, result)
        }));
    }
    drop(entered);
    for _ in 0..64 {
        entries.recv().await.ok_or(ControlError::Capacity)?;
    }
    assert_eq!(signaling.playbacks.len(), 64);
    for handle in &handles {
        assert_eq!(handle.is_finished(), readiness == Readiness::Completed);
    }
    Ok(handles)
}

async fn exercise(
    config: Config,
    storage: crate::storage::StoragePort,
    readiness: Readiness,
) -> Result<(), ControlError> {
    let catalog = storage
        .prepare_music(config.music_on_hold.clone())
        .await
        .map_err(failure)?;
    let (controller, runner, effects) = Controller::new(config.clone(), 2);
    let runner = runner.with_music(catalog);
    let (media, hub, notices) = MediaHub::new(2);
    let mut signaling =
        Signaling::bind(config, controller.clone(), effects, media.clone(), notices)
            .await?
            .with_storage(storage);
    let listeners = signaling.listeners()?;
    let peer = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let sockets = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = sockets.local_addr().map_err(failure)?;
    let key_id = id("music-admission")?;
    let playback_id = id("completion-sentinel")?;
    let (release, _) = watch::channel(false);
    let runner = tokio::spawn(runner.run());
    let hub = tokio::spawn(hub.run(controller.shutdown_signal()));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut subscription = controller
            .subscribe(credentials()?, None, "app".into())
            .await?;
        let key = controller
            .incoming(IncomingCall {
                id: key_id.clone(),
                endpoint: "phone".into(),
                caller: "100".into(),
                context: "internal".into(),
                extension: "100".into(),
                target: IncomingTarget::Stasis {
                    application: "app".into(),
                    arguments: Vec::new(),
                },
            })
            .await?;
        controller
            .signal(Signal::Milestone {
                key: key.clone(),
                milestone: Milestone::Answered,
            })
            .await?;
        answered(&mut subscription.events, &key).await?;
        media
            .attach(
                key.clone(),
                sockets,
                RtpSession {
                    remote: peer.local_addr().map_err(failure)?,
                    format: AudioFormat::Pcmu,
                    payload_type: 0,
                    telephone: None,
                },
                PeerMediaDirection::SendReceive,
            )
            .await
            .map_err(failure)?;
        signaling.calls.insert(
            key.clone(),
            Active {
                listener: 0,
                media: super::MediaAdmission::Attached,
                digits: DigitDelivery::Media,
                direction_reply: None,
            },
        );
        controller
            .execute(
                credentials()?,
                None,
                Action::CreateBridge {
                    id: id("sentinel")?,
                    name: "sentinel".into(),
                },
            )
            .await?;
        controller
            .execute(
                credentials()?,
                None,
                Action::PlayBridge {
                    bridge: id("sentinel")?,
                    id: playback_id.clone(),
                    media: MediaUri::try_from("sound:known").map_err(failure)?,
                    language: None,
                    options: Default::default(),
                },
            )
            .await?;
        let effect = signaling
            .effects
            .recv()
            .await
            .ok_or(ControlError::Capacity)?;
        let Effect::PlayMedia { owner, token, .. } = effect else {
            return Err(ControlError::Invalid("Expected the sentinel playback"));
        };
        let handles = fill_jobs(&mut signaling, readiness, owner, token, &release).await?;
        let accepted = controller
            .execute(
                credentials()?,
                None,
                Action::StartMusic {
                    channel: key.id.clone(),
                    class: None,
                },
            )
            .await?;
        assert_eq!(accepted.status, 204);
        let effect = signaling
            .effects
            .recv()
            .await
            .ok_or(ControlError::Capacity)?;
        let Effect::StartMusic {
            key: owner, token, ..
        } = &effect
        else {
            return Err(ControlError::Invalid("Expected accepted music admission"));
        };
        assert_eq!(owner, &key);
        let music = *token;
        signaling.effect(effect, 0).await?;
        assert!(signaling.playbacks.len() <= 64);
        if readiness == Readiness::Completed {
            serve_until_completion(&mut signaling, &mut subscription.events, &playback_id).await?;
        } else {
            assert_eq!(signaling.playbacks.len(), 64);
        }
        let finished =
            completion_marker(&controller, &mut subscription.events, &key, &playback_id).await?;
        assert_eq!(finished, 0);
        let stopped = controller
            .execute(credentials()?, None, Action::StopMusic(key.id.clone()))
            .await?;
        assert_eq!(stopped.status, 204);
        match readiness {
            Readiness::Completed => {
                let effect = signaling.effects.try_recv().map_err(|error| {
                    failure(format!(
                        "Completed actor tasks blocked accepted music: {error}"
                    ))
                })?;
                if let Effect::ControlPlayback { token, operation } = &effect {
                    assert_eq!(*token, music);
                    assert_eq!(*operation, crate::media::PlaybackControl::Stop);
                } else {
                    return Err(ControlError::Invalid("Admitted music lost its token"));
                }
                signaling.effect(effect, 1).await?;
            }
            Readiness::Live => {
                assert!(matches!(
                    signaling.effects.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
                assert!(handles.iter().all(|handle| !handle.is_finished()));
            }
        }
        Ok(())
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    release.send_replace(true);
    signaling.playbacks.abort_all();
    let mut result = result;
    let drained = tokio::time::timeout(Duration::from_secs(3), async {
        let mut cleanup = Ok(());
        while let Some(completed) = signaling.playbacks.join_next().await {
            cleanup = combine(
                cleanup,
                match completed {
                    Ok((_owner, _token, result)) => result,
                    Err(error) if error.is_cancelled() => Ok(()),
                    Err(error) => Err(failure(error)),
                },
            );
        }
        cleanup
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    result = combine(result, drained);
    result = combine(result, signaling.network.close().await);
    controller.shutdown();
    result = combine(result, joined(runner).await);
    result = combine(result, joined(hub).await);
    drop(signaling);
    for (_, address) in listeners {
        result = combine(
            result,
            tokio::net::UdpSocket::bind(address)
                .await
                .map(drop)
                .map_err(|error| failure(format!("SIP UDP listener rebind failed: {error}"))),
        );
    }
    let rebound = local
        .port()
        .checked_add(1)
        .ok_or(ControlError::Capacity)
        .and_then(|end| {
            SocketPair::bind(local.ip(), local.port(), end)
                .map(drop)
                .map_err(|error| failure(format!("Playback RTP/RTCP rebind failed: {error}")))
        });
    combine(result, rebound)
}
