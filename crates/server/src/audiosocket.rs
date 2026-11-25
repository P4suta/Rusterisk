use crate::clock::Clock;
use crate::media_link::{Channels, DigitSchedule, DigitStep, LinkStats};
use rusterisk_core::{DtmfDigit, DtmfTone};
use rusterisk_media::{AudioSocket, AudioSocketDecoder, MediaError, PcmFramer};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

#[cfg(test)]
mod shutdown_tests;

enum LinkTermination {
    Local(LinkStats),
    Peer(LinkStats),
}

pub struct AudioSocketLink {
    socket: TcpStream,
    channels: Channels,
    decoder: AudioSocketDecoder,
    audio: PcmFramer,
    rate: u32,
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

async fn send(socket: &mut TcpStream, message: AudioSocket) -> Result<(), MediaError> {
    let bytes = message.encode()?;
    tokio::time::timeout(Duration::from_millis(100), socket.write_all(&bytes))
        .await
        .map_err(failure)?
        .map_err(failure)
}

impl AudioSocketLink {
    pub fn new(socket: TcpStream, channels: Channels, rate: u32) -> Result<Self, MediaError> {
        Ok(Self {
            socket,
            channels,
            decoder: AudioSocketDecoder::new(),
            audio: PcmFramer::new(rate)?,
            rate,
        })
    }

    async fn serve(&mut self) -> Result<LinkTermination, MediaError> {
        let mut stats = LinkStats::default();
        let mut input = [0; 1923];
        let period = Duration::from_millis(20);
        let started = tokio::time::Instant::now();
        let mut tick = tokio::time::interval_at(started + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let clock = Clock::start();
        let mut digits = DigitSchedule::default();
        loop {
            if *self.channels.stop.borrow() || self.channels.incoming.is_closed() {
                break;
            }
            tokio::select! {
                changed = self.channels.stop.changed() => if changed.is_err() || *self.channels.stop.borrow() { break; },
                changed = self.channels.direction.changed() => {
                    if changed.is_err() { break; }
                    self.audio.clear();
                }
                size = self.socket.read(&mut input) => {
                    let size = size.map_err(failure)?;
                    if size == 0 {
                        if !self.decoder.is_empty() { return Err(MediaError::InvalidPacket); }
                        return Ok(LinkTermination::Peer(stats));
                    }
                    let mut offset = 0;
                    while offset < size {
                        let (consumed, message) = self.decoder.push(&input[offset..size])?;
                        offset += consumed;
                        if let Some(message) = message {
                            stats.packets_received += 1;
                            match message {
                                AudioSocket::Terminate => return Ok(LinkTermination::Peer(stats)),
                                AudioSocket::Uuid(_) => return Err(MediaError::InvalidPacket),
                                AudioSocket::Audio(frame) => {
                                    if frame.rate() != self.rate { return Err(MediaError::InvalidFrame); }
                                    if self.channels.direction.borrow().can_send() { self.audio.push(frame)?; }
                                }
                                AudioSocket::Dtmf(symbol) => {
                                    let digit = DtmfDigit::from_symbol(char::from(symbol)).ok_or(MediaError::InvalidPacket)?;
                                    if self.channels.direction.borrow().can_send() {
                                        self.channels.digits.try_send(DtmfTone { digit, duration_ms: 0 }).map_err(|_| MediaError::Capacity)?;
                                    }
                                }
                            }
                        }
                    }
                }
                frame = self.channels.outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    if self.channels.direction.borrow().can_receive() {
                        send(&mut self.socket, AudioSocket::Audio(frame.resample(self.rate)?)).await?;
                        stats.packets_sent += 1;
                    }
                }
                Some(command) = self.channels.commands.recv(), if digits.idle() => digits.admit(command)?,
                () = clock.reached(digits.wake()) => {
                    match digits.advance(clock.now_ms().map_err(failure)?, true)? {
                        DigitStep::Send(tone) if self.channels.direction.borrow().can_receive() => {
                            send(&mut self.socket, AudioSocket::Dtmf(tone.digit.symbol() as u8)).await?;
                            stats.packets_sent += 1;
                        }
                        DigitStep::Finished(token) => self.channels.completed.try_send(token).map_err(|_| MediaError::Capacity)?,
                        DigitStep::Waiting | DigitStep::Send(_) => {}
                    }
                }
                _instant = tick.tick() => {
                    if let Some(frame) = self.audio.pop() {
                        match self.channels.incoming.try_send(frame) {
                            Ok(()) => {},
                            Err(mpsc::error::TrySendError::Full(_)) => stats.dropped += 1,
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                }
            }
        }
        Ok(LinkTermination::Local(stats))
    }

    pub async fn run(mut self) -> Result<LinkStats, MediaError> {
        let result = match self.serve().await {
            Ok(LinkTermination::Peer(stats)) => return Ok(stats),
            Ok(LinkTermination::Local(stats)) => Ok(stats),
            Err(error) => Err(error),
        };
        let cleanup = send(&mut self.socket, AudioSocket::Terminate).await;
        match (result, cleanup) {
            (Ok(stats), Ok(())) => Ok(stats),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(failure(format!(
                "{primary}; stream cleanup failed: {cleanup}"
            ))),
        }
    }
}
