// Drives one owned WebRTC leg on its own 20 millisecond clock through the channels of a task link.
// The media task runs legs in its turn, and tests use this driver to reach one leg alone.
// The module that includes this file imports the names that `super` lists.
use super::{Channels, MediaError, WebRtcLeg, WebRtcSession, WebRtcStats, MAX_PACKET};
use std::time::Duration;

pub async fn run(
    session: WebRtcSession,
    mut channels: Channels,
) -> Result<WebRtcStats, MediaError> {
    let direction = *channels.direction.borrow_and_update();
    let mut leg = WebRtcLeg::new(session, direction)?;
    let period = Duration::from_millis(20);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scratch = [0; MAX_PACKET + 1];
    let mut tones = Vec::new();
    let mut finished = Vec::new();
    let result = loop {
        if *channels.stop.borrow() || channels.incoming.is_closed() || leg.closed() {
            break Ok(());
        }
        tokio::select! {
            changed = channels.stop.changed() => {
                if changed.is_err() || *channels.stop.borrow() { break Ok(()); }
            }
            changed = channels.direction.changed() => {
                if changed.is_err() { break Ok(()); }
                let direction = *channels.direction.borrow_and_update();
                if let Err(error) = leg.set_direction(direction) { break Err(error); }
            }
            command = channels.commands.recv(), if !channels.commands.is_closed() || !channels.commands.is_empty() => {
                if let Some(command) = command {
                    if let Err(error) = leg.queue_digits(command) { break Err(error); }
                }
            }
            frame = channels.outgoing.recv() => {
                let Some(frame) = frame else { break Ok(()); };
                // A direction change that raced the frame applies before the frame leaves.
                if channels.direction.has_changed().unwrap_or(false) {
                    let direction = *channels.direction.borrow_and_update();
                    if let Err(error) = leg.set_direction(direction) { break Err(error); }
                }
                if let Err(error) = leg.send(frame) { break Err(error); }
            }
            _instant = tick.tick() => {
                match turn(&mut leg, &mut scratch, &mut tones, &mut finished, &mut channels) {
                    Ok(true) => {}
                    Ok(false) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
        }
    };
    let stats = leg.finish();
    result.map(|()| stats)
}

// Runs one turn of the leg, and reports false once the media side stopped taking frames.
fn turn(
    leg: &mut WebRtcLeg,
    scratch: &mut [u8; MAX_PACKET + 1],
    tones: &mut Vec<rusterisk_core::DtmfTone>,
    finished: &mut Vec<rusterisk_core::DtmfToken>,
    channels: &mut Channels,
) -> Result<bool, MediaError> {
    leg.receive(scratch)?;
    let frame = leg.advance(tones, finished)?;
    for tone in tones.drain(..) {
        channels
            .digits
            .try_send(tone)
            .map_err(|_| MediaError::Capacity)?;
    }
    for token in finished.drain(..) {
        channels
            .completed
            .try_send(token)
            .map_err(|_| MediaError::Capacity)?;
    }
    if let Some(frame) = frame {
        if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) =
            channels.incoming.try_send(frame)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
