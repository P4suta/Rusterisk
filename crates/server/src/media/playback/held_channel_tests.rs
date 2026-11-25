use super::{PlaybackControl, Source};
use crate::media::{Command, DigitSupport, MediaHub, Port};
use crate::storage::{PlaybackMedia, StoragePort, StorageWorker};
use crate::{CallKey, ControlError};
use rusterisk_api::ChannelId;
use rusterisk_core::{
    AudioGate, Call, CallEvent, MediaTaskKind, MediaTaskToken, MediaTasks, MuteDirection,
    PeerMediaDirection,
};
use rusterisk_media::{AudioTarget, Frame, Gate};
use rusterisk_storage::Recorder;
use std::time::Duration;
use tokio::sync::oneshot;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; held channel playback cleanup failed: {cleanup}"
        ))),
    }
}

fn key(name: &str, generation: u64) -> Result<CallKey, ControlError> {
    Ok(CallKey {
        id: ChannelId::try_from(name.to_owned())?,
        generation,
    })
}

fn frame(sample: i16) -> Result<Frame, ControlError> {
    Frame::new(&[sample; 960], 48000).map_err(failure)
}

fn consumed(hub: &MediaHub, token: MediaTaskToken) -> Result<u64, ControlError> {
    let entry = hub
        .playbacks
        .entries
        .get(&token)
        .ok_or(ControlError::Invalid(
            "Playback ended before consuming audio",
        ))?;
    if let Source::Attached { source, .. } = &entry.source {
        Ok(source.sample_position())
    } else {
        Err(ControlError::Invalid("Playback source was not attached"))
    }
}

#[tokio::test]
async fn held_conversation_does_not_silence_consumed_channel_playback() -> Result<(), ControlError>
{
    fixture(false).await
}

#[tokio::test]
async fn outgoing_mute_still_silences_consumed_channel_playback_while_held(
) -> Result<(), ControlError> {
    fixture(true).await
}

async fn fixture(muted: bool) -> Result<(), ControlError> {
    let directory = tempfile::tempdir().map_err(failure)?;
    let result = async {
        let mut recorder =
            Recorder::create(directory.path(), "held-announcement", 8000).map_err(failure)?;
        for _ in 0..4 {
            recorder
                .write(Frame::new(&[3600; 160], 8000).map_err(failure)?)
                .map_err(failure)?;
        }
        recorder.finish().map_err(failure)?;
        let (storage, worker) = StorageWorker::start(directory.path().to_owned())
            .await
            .map_err(failure)?;
        let result = exercise(storage, muted).await;
        combine(result, worker.close().await.map_err(failure))
    }
    .await;
    combine(result, directory.close().map_err(failure))
}

async fn exercise(storage: StoragePort, muted: bool) -> Result<(), ControlError> {
    let (_media, mut hub, _notices) = MediaHub::new(2);
    let channel = key("held-playback", 7)?;
    let peer = key("bridge-peer", 8)?;
    let mut tasks = MediaTasks::new(channel.generation);
    let token = tasks.reserve(MediaTaskKind::Playback)?;
    tasks.start(token)?;
    let (completed, mut completion) = oneshot::channel();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let held = Call::incoming(channel.generation)
            .apply(CallEvent::Answer)
            .call()
            .hold()?;
        let held = if muted {
            held.mute(MuteDirection::Outgoing)?
        } else {
            held
        };
        if held.audio_gate() != AudioGate::Closed {
            return Err(ControlError::Invalid("HOLD left conversation audio open"));
        }
        let (generated_audio_gate, expected) = if muted {
            if held.generated_audio_gate() != AudioGate::OutgoingMuted {
                return Err(ControlError::Invalid("HOLD lost outgoing muting"));
            }
            (Gate::OutgoingMuted, 0)
        } else {
            if held.generated_audio_gate() != AudioGate::Open {
                return Err(ControlError::Invalid(
                    "HOLD closed generated audio permission",
                ));
            }
            (Gate::Open, 3600)
        };
        let (port, mut channels) =
            Port::new(PeerMediaDirection::SendReceive, DigitSupport::Unsupported);
        hub.install_link(channel.clone(), port, Gate::Open, 48000)
            .map_err(failure)?;
        let (port, mut peers) =
            Port::new(PeerMediaDirection::SendReceive, DigitSupport::Unsupported);
        hub.install_link(peer.clone(), port, Gate::Open, 48000)
            .map_err(failure)?;
        hub.mixer
            .bridge(10, &[channel.clone(), peer])
            .map_err(failure)?;
        hub.command(Command::Gate {
            key: channel.clone(),
            gate: Gate::Closed,
            generated_audio_gate,
        })
        .map_err(failure)?;
        hub.playbacks
            .reserve(
                token,
                AudioTarget::Channel(channel.clone()),
                completed,
                Default::default(),
            )
            .map_err(failure)?;
        let source = storage
            .play(PlaybackMedia::Recording(ChannelId::try_from(
                "held-announcement".to_owned(),
            )?))
            .await
            .map_err(failure)?;
        hub.playbacks.attach(token, source).map_err(failure)?;
        loop {
            let before = consumed(&hub, token)?;
            channels.incoming.try_send(frame(4000)?).map_err(failure)?;
            peers.incoming.try_send(frame(-7000)?).map_err(failure)?;
            hub.tick().map_err(failure)?;
            let output = channels.outgoing.try_recv().map_err(failure)?;
            let peer_output = peers.outgoing.try_recv().map_err(failure)?;
            if consumed(&hub, token)? > before {
                if output.samples() != [expected; 960] {
                    return Err(ControlError::Invalid(
                        "Consumed channel playback ignored its generated audio permission",
                    ));
                }
                if peer_output.samples() != [0; 960] {
                    return Err(ControlError::Invalid(
                        "Held conversation or channel playback reached the bridge peer",
                    ));
                }
                if hub
                    .playbacks
                    .is_music(&AudioTarget::Channel(channel.clone()))
                {
                    return Err(ControlError::Invalid("Ordinary playback became Music"));
                }
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(failure)
    .and_then(|result| result);
    hub.playbacks.control(token, PlaybackControl::Stop);
    let stopped = completion
        .try_recv()
        .map_err(failure)
        .and_then(|result| result.map_err(failure));
    let mut result = combine(result, stopped);
    if !tasks.finish(token) {
        result = combine(
            result,
            Err(ControlError::Invalid("Playback quota was not released")),
        );
    }
    combine(result, hub.close().await.map_err(failure))
}
