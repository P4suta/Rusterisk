// Drives one owned media leg on its own 20 millisecond clock through the channels of a task link.
// The media task runs legs in its turn, and tests use this driver to reach one leg alone.
// The module that includes this file imports the names that `super` lists.
use super::{
    ControlReception, LinkStats, Measurements, MediaError, RtpChannels, RtpLeg, RtpSession,
    SocketPair, MAX_PACKET,
};
use std::time::Duration;
use tokio::sync::watch;

pub struct RtpLink {
    pub(super) leg: RtpLeg,
    channels: RtpChannels,
    pub(super) measurements: watch::Sender<Measurements>,
}

impl RtpLink {
    pub fn new(
        sockets: SocketPair,
        session: RtpSession,
        channels: RtpChannels,
    ) -> Result<Self, MediaError> {
        let direction = *channels.direction.borrow();
        Ok(Self {
            leg: RtpLeg::new(sockets, session, direction)?,
            channels,
            measurements: watch::channel(Measurements::default()).0,
        })
    }

    pub async fn run(self) -> Result<LinkStats, MediaError> {
        self.serve(|_| Ok(())).await
    }

    pub async fn serve(
        mut self,
        mut receipt: impl FnMut(ControlReception) -> Result<(), MediaError>,
    ) -> Result<LinkStats, MediaError> {
        let period = Duration::from_millis(20);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut scratch = [0; MAX_PACKET + 1];
        let mut tones = Vec::new();
        loop {
            if *self.channels.stop.borrow() || self.channels.incoming.is_closed() {
                break;
            }
            tokio::select! {
                changed = self.channels.stop.changed() => {
                    if changed.is_err() || *self.channels.stop.borrow() { break; }
                }
                changed = self.channels.direction.changed() => {
                    if changed.is_err() { break; }
                    let direction = *self.channels.direction.borrow_and_update();
                    self.leg.set_direction(direction);
                }
                command = self.channels.commands.recv(), if !self.channels.commands.is_closed() || !self.channels.commands.is_empty() => {
                    if let Some(command) = command {
                        self.leg.queue_digits(command)?;
                    }
                }
                frame = self.channels.outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    // A direction change that raced the frame applies before the frame leaves.
                    if self.channels.direction.has_changed().unwrap_or(false) {
                        let direction = *self.channels.direction.borrow_and_update();
                        self.leg.set_direction(direction);
                    }
                    self.leg.send(frame)?;
                }
                _instant = tick.tick() => {
                    self.leg.receive(&mut scratch, &mut tones, &mut receipt)?;
                    let advanced = self.leg.advance(&mut tones)?;
                    for tone in tones.drain(..) {
                        self.channels.digits.try_send(tone).map_err(|_| MediaError::Capacity)?;
                    }
                    if let Some(token) = advanced.completed {
                        self.channels.completed.try_send(token).map_err(|_| MediaError::Capacity)?;
                    }
                    if let Some(frame) = advanced.frame {
                        if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = self.channels.incoming.try_send(frame) {
                            break;
                        }
                    }
                }
            }
            self.measurements.send_replace(self.leg.measurements());
        }
        Ok(self.leg.finish())
    }
}
