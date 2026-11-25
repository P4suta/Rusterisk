use super::{failure, sockets};
use rusterisk_config::Codec;
use rusterisk_core::{DtmfDigit, DtmfTone, PeerMediaDirection};
use rusterisk_media::{Frame, MediaError};
use rusterisk_test_support::Wait;
#[path = "../webrtc_leg/driver.rs"]
mod webrtc_driver;

use rusterisk_media::MAX_PACKET;
use rusterisk_server::{
    media_link::{Channels, DigitCommand},
    webrtc::{WebRtcLeg, WebRtcOffer, WebRtcSession, WebRtcStats},
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

struct Application {
    audio: mpsc::Receiver<Frame>,
    outgoing: mpsc::Sender<Frame>,
    digits: mpsc::Receiver<DtmfTone>,
    commands: mpsc::Sender<DigitCommand>,
    stop: watch::Sender<bool>,
    direction: watch::Sender<PeerMediaDirection>,
}

fn channels() -> (Channels, Application) {
    let (input, audio) = mpsc::channel(3);
    let (outgoing, output) = mpsc::channel(3);
    let (digits, received) = mpsc::channel(16);
    let (commands, pending) = mpsc::channel(8);
    let (completed, _completed) = mpsc::channel(8);
    let (stop, stopped) = watch::channel(false);
    let (direction, directed) = watch::channel(PeerMediaDirection::SendReceive);
    (
        Channels {
            incoming: input,
            outgoing: output,
            digits,
            commands: pending,
            completed,
            stop: stopped,
            direction: directed,
        },
        Application {
            audio,
            outgoing,
            digits: received,
            commands,
            stop,
            direction,
        },
    )
}

fn only_eight_khz(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    for line in source.lines() {
        if line.starts_with("a=rtpmap:102 ") || line.starts_with("a=fmtp:102 ") {
            continue;
        }
        if line.starts_with("m=audio ") {
            let fields: Vec<&str> = line
                .split_whitespace()
                .filter(|field| *field != "102")
                .collect();
            output.push_str(&fields.join(" "));
        } else {
            output.push_str(line);
        }
        output.push_str("\r\n");
    }
    output
}

async fn finish(
    mut task: tokio::task::JoinHandle<Result<WebRtcStats, MediaError>>,
) -> Result<WebRtcStats, MediaError> {
    match Wait::Ready.bounded(&mut task).await {
        Ok(result) => result.map_err(failure)?,
        Err(primary) => {
            task.abort();
            match task.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; cleanup failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!("{primary}; worker failed: {error}"))),
                Ok(Ok(_)) => Err(failure(primary)),
            }
        }
    }
}

fn sessions(codec: Codec, clock: u32) -> Result<(WebRtcSession, WebRtcSession), MediaError> {
    let socket = sockets()?;
    let local = socket.local_addr().map_err(failure)?;
    let offered_codecs = match (codec, clock) {
        (Codec::Opus, 8000) => &[Codec::Opus, Codec::Pcmu][..],
        _ => std::slice::from_ref(&codec),
    };
    let offer = WebRtcOffer::new(socket, local, offered_codecs, Instant::now())?;
    let description = match clock {
        8000 => only_eight_khz(offer.description()),
        48000 => offer.description().to_owned(),
        _ => return Err(MediaError::Unsupported),
    };
    let socket = sockets()?;
    let local = socket.local_addr().map_err(failure)?;
    let (right, answer) =
        WebRtcSession::answer(socket, local, &description, &[codec], Instant::now())?;
    let left = offer.accept_answer(&answer)?;
    assert_eq!(
        left.telephone().ok_or(MediaError::Unsupported)?.rate(),
        clock
    );
    assert_eq!(
        right.telephone().ok_or(MediaError::Unsupported)?.rate(),
        clock
    );
    Ok((left, right))
}

async fn ready(left: &mut Application, right: &mut Application) -> Result<(), MediaError> {
    let mut ready = [false; 2];
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !ready.iter().all(|ready| *ready) {
        tokio::select! {
            _instant = tick.tick() => {
                left.outgoing.try_send(Frame::new(&[4000; 960], 48000)?).map_err(failure)?;
                right.outgoing.try_send(Frame::new(&[-7000; 960], 48000)?).map_err(failure)?;
            }
            frame = left.audio.recv() => { frame.ok_or(MediaError::UnknownStream)?; ready[0] = true; }
            frame = right.audio.recv() => { frame.ok_or(MediaError::UnknownStream)?; ready[1] = true; }
        }
    }
    Ok(())
}

async fn exchange(codec: Codec, duration_ms: u32, clock: u32) -> Result<(), MediaError> {
    let (left, right) = sessions(codec, clock)?;
    let (left_channels, mut left_app) = channels();
    let (right_channels, mut right_app) = channels();
    let left = tokio::spawn(webrtc_driver::run(left, left_channels));
    let right = tokio::spawn(webrtc_driver::run(right, right_channels));
    let mut phase = "audio readiness";
    let result = Wait::Ready
        .bounded(async {
            ready(&mut left_app, &mut right_app).await?;
            let left_tone = DtmfTone {
                digit: DtmfDigit::Hash,
                duration_ms,
            };
            let right_tone = DtmfTone {
                digit: DtmfDigit::D,
                duration_ms,
            };
            left_app
                .commands
                .try_send(DigitCommand::Tone(left_tone))
                .map_err(failure)?;
            right_app
                .commands
                .try_send(DigitCommand::Tone(right_tone))
                .map_err(failure)?;
            phase = "digit delivery";
            let (received_left, received_right) =
                tokio::join!(left_app.digits.recv(), right_app.digits.recv());
            assert_eq!(received_left.ok_or(MediaError::UnknownStream)?, right_tone);
            assert_eq!(received_right.ok_or(MediaError::UnknownStream)?, left_tone);
            let (left_duplicate, right_duplicate) = tokio::join!(
                Wait::Quiet.bounded(left_app.digits.recv()),
                Wait::Quiet.bounded(right_app.digits.recv()),
            );
            assert!(left_duplicate.is_err());
            assert!(right_duplicate.is_err());
            Ok::<(), MediaError>(())
        })
        .await
        .map_err(|error| failure(format!("{phase}: {error}")));
    left_app.stop.send_replace(true);
    right_app.stop.send_replace(true);
    let (left, right) = tokio::join!(finish(left), finish(right));
    if let Err(error) = result {
        return Err(failure(format!(
            "{error}; left: {left:?}; right: {right:?}"
        )));
    }
    result??;
    for stats in [left?, right?] {
        assert!(stats.transport_connected);
        assert!(stats.audio.packets_received > 0);
    }
    Ok(())
}

#[tokio::test]
async fn authenticated_digit_ends_are_received_once_with_each_audio_codec() -> Result<(), MediaError>
{
    for (codec, clock) in [
        (Codec::Pcmu, 8000),
        (Codec::Pcma, 8000),
        (Codec::Opus, 48000),
    ] {
        exchange(codec, 60, clock).await?;
    }
    Ok(())
}

#[tokio::test]
async fn one_millisecond_tones_remain_valid_on_an_independent_event_clock() -> Result<(), MediaError>
{
    exchange(Codec::Opus, 1, 8000).await
}

#[tokio::test]
async fn received_events_started_during_hold_do_not_complete_after_resume() -> Result<(), MediaError>
{
    let (left, right) = sessions(Codec::Pcmu, 8000)?;
    let (left_channels, mut left_app) = channels();
    let (right_channels, mut right_app) = channels();
    let left = tokio::spawn(webrtc_driver::run(left, left_channels));
    let right = tokio::spawn(webrtc_driver::run(right, right_channels));
    let result = Wait::Ready
        .bounded(async {
            ready(&mut left_app, &mut right_app).await?;
            left_app
                .direction
                .send_replace(PeerMediaDirection::ReceiveOnly);
            right_app
                .commands
                .try_send(DigitCommand::Tone(DtmfTone {
                    digit: DtmfDigit::Hash,
                    duration_ms: 500,
                }))
                .map_err(failure)?;
            if Wait::Observation
                .bounded(left_app.digits.recv())
                .await
                .is_ok()
            {
                return Err(failure(
                    "A digit was delivered while incoming media was held",
                ));
            }
            left_app
                .direction
                .send_replace(PeerMediaDirection::SendReceive);
            if Wait::Observation
                .bounded(left_app.digits.recv())
                .await
                .is_ok()
            {
                return Err(failure(
                    "An event started during hold completed after media resumed",
                ));
            }
            let tone = DtmfTone {
                digit: DtmfDigit::Two,
                duration_ms: 60,
            };
            right_app
                .commands
                .try_send(DigitCommand::Tone(tone))
                .map_err(failure)?;
            if left_app
                .digits
                .recv()
                .await
                .ok_or(MediaError::UnknownStream)?
                != tone
            {
                return Err(failure(
                    "A different event was delivered after media resumed",
                ));
            }
            Ok::<(), MediaError>(())
        })
        .await
        .map_err(failure);
    left_app.stop.send_replace(true);
    right_app.stop.send_replace(true);
    let (left, right) = tokio::join!(finish(left), finish(right));
    result??;
    assert!(left?.transport_connected);
    assert!(right?.transport_connected);
    Ok(())
}
