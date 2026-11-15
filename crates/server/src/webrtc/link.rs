use super::{negotiation_error, WebRtcSession};
use crate::media_link::{Channels, LinkStats};
use rusterisk_config::Codec;
use rusterisk_media::{
    alaw_encode, mulaw_encode, AudioDecoder, AudioFormat, Frame, MediaError, OpusCodec, MAX_PACKET,
};
use std::{
    mem,
    time::{Duration, Instant},
};
use str0m::{
    media::{MediaData, MediaTime},
    net::{Protocol, Receive},
    Event, Input, Output,
};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Default)]
pub struct WebRtcStats {
    pub transport_connected: bool,
    pub audio: LinkStats,
}

enum Encoder {
    Pcmu,
    Pcma,
    Opus(OpusCodec),
}

impl Encoder {
    fn new(codec: Codec) -> Result<Self, MediaError> {
        match codec {
            Codec::Pcmu => Ok(Self::Pcmu),
            Codec::Pcma => Ok(Self::Pcma),
            Codec::Opus => Ok(Self::Opus(OpusCodec::new()?)),
        }
    }

    fn encode(&mut self, frame: Frame) -> Result<Vec<u8>, MediaError> {
        match self {
            Self::Pcmu => Ok(frame
                .resample(8000)?
                .samples()
                .iter()
                .map(|sample| mulaw_encode(*sample))
                .collect()),
            Self::Pcma => Ok(frame
                .resample(8000)?
                .samples()
                .iter()
                .map(|sample| alaw_encode(*sample))
                .collect()),
            Self::Opus(codec) => codec.encode(frame),
        }
    }
}

fn format(codec: str0m::format::Codec) -> Result<AudioFormat, MediaError> {
    match codec {
        str0m::format::Codec::PCMU => Ok(AudioFormat::Pcmu),
        str0m::format::Codec::PCMA => Ok(AudioFormat::Pcma),
        str0m::format::Codec::Opus => Ok(AudioFormat::Opus),
        str0m::format::Codec::G722
        | str0m::format::Codec::CN
        | str0m::format::Codec::AmrWb
        | str0m::format::Codec::H264
        | str0m::format::Codec::H265
        | str0m::format::Codec::H266
        | str0m::format::Codec::Vp8
        | str0m::format::Codec::Vp9
        | str0m::format::Codec::Av1
        | str0m::format::Codec::Tele
        | str0m::format::Codec::Rtx
        | str0m::format::Codec::Red
        | str0m::format::Codec::Null
        | str0m::format::Codec::Unknown => Err(MediaError::Unsupported),
        _ => Err(MediaError::Unsupported),
    }
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

struct Link {
    session: WebRtcSession,
    channels: Channels,
    encoder: Encoder,
    decoder: AudioDecoder,
    decoded_format: AudioFormat,
    stats: WebRtcStats,
    started: Instant,
}

impl Link {
    fn receive(&mut self, data: MediaData) -> Result<(), MediaError> {
        if *self.channels.stop.borrow() || !self.channels.direction.borrow().can_send() {
            self.decoder.clear();
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        let format = format(data.params.spec().codec)?;
        if format != self.decoded_format {
            self.decoder.reset(format)?;
            self.decoded_format = format;
        }
        if !data.contiguous {
            self.decoder.clear();
        }
        if let Err(error) = self.decoder.push(data.data.as_ref()) {
            self.decoder.clear();
            self.stats.audio.dropped += 1;
            return match error {
                MediaError::InvalidFrame
                | MediaError::InvalidPacket
                | MediaError::Capacity
                | MediaError::Codec(_) => Ok(()),
                error @ (MediaError::InvalidRoute
                | MediaError::UnknownStream
                | MediaError::Unsupported
                | MediaError::Transport(_)
                | MediaError::Control(_)) => Err(error),
            };
        }
        self.stats.audio.packets_received += 1;
        while let Some(frame) = self.decoder.pop() {
            match self.channels.incoming.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => self.stats.audio.dropped += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(MediaError::UnknownStream),
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<bool, MediaError> {
        let mut closed = false;
        for _ in 0..256 {
            match self
                .session
                .engine
                .rtc
                .poll_output()
                .map_err(negotiation_error)?
            {
                Output::Timeout(timeout) => {
                    self.session.engine.timeout = timeout;
                    return Ok(!closed);
                }
                Output::Transmit(transmit) => {
                    if self.session.engine.transmits.len() >= 32
                        || transmit.contents.len() > MAX_PACKET
                    {
                        return Err(MediaError::Capacity);
                    }
                    self.session.engine.transmits.push(transmit);
                }
                Output::Event(Event::Connected) => self.stats.transport_connected = true,
                Output::Event(Event::Closed) => closed = true,
                Output::Event(event) => {
                    if let Some(data) = self.session.engine.metadata(event)? {
                        self.receive(data)?;
                    }
                }
            }
        }
        Err(MediaError::Capacity)
    }

    async fn flush(&mut self) -> Result<(), MediaError> {
        let result = tokio::time::timeout(Duration::from_millis(20), async {
            for transmit in self.session.engine.transmits.drain(..) {
                if transmit.proto != Protocol::Udp {
                    return Err(MediaError::Unsupported);
                }
                let size = self
                    .session
                    .engine
                    .sockets
                    .data()
                    .send_to(&transmit.contents, transmit.destination)
                    .await
                    .map_err(failure)?;
                if size != transmit.contents.len() {
                    return Err(MediaError::Transport("Incomplete WebRTC datagram".into()));
                }
            }
            Ok::<(), MediaError>(())
        })
        .await
        .map_err(failure)?;
        result
    }

    fn write(&mut self, frame: Frame) -> Result<(), MediaError> {
        if !self.channels.direction.borrow().can_receive()
            || !self.session.engine.rtc.is_connected()
        {
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        let payload = self.encoder.encode(frame)?;
        let now = Instant::now();
        let mid = self.session.engine.mid.ok_or(MediaError::InvalidRoute)?;
        self.session
            .engine
            .rtc
            .writer(mid)
            .ok_or(MediaError::InvalidRoute)?
            .write(
                self.session.payload,
                now,
                MediaTime::from(now.duration_since(self.started)),
                payload,
            )
            .map_err(negotiation_error)?;
        self.stats.audio.packets_sent += 1;
        Ok(())
    }

    async fn serve(&mut self) -> Result<(), MediaError> {
        let mut bytes = [0; MAX_PACKET + 1];
        let handshake_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if *self.channels.stop.borrow() || self.channels.incoming.is_closed() {
                return Ok(());
            }
            let alive = self.drain()?;
            self.flush().await?;
            if !alive {
                return Ok(());
            }
            tokio::select! {
                changed = self.channels.stop.changed() => if changed.is_err() || *self.channels.stop.borrow() { return Ok(()); },
                changed = self.channels.direction.changed() => { if changed.is_err() { return Ok(()); } self.decoder.clear(); }
                result = self.session.engine.sockets.data().recv_from(&mut bytes) => {
                    let (size, source) = result.map_err(failure)?;
                    if size > MAX_PACKET { self.stats.audio.dropped += 1; continue; }
                    let contents = match bytes[..size].try_into() { Ok(contents) => contents, Err(_) => { self.stats.audio.dropped += 1; continue; } };
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: self.session.engine.sockets.local_addr().map_err(failure)?, contents });
                    if !self.session.engine.rtc.accepts(&input) { self.stats.audio.dropped += 1; continue; }
                    self.session.engine.rtc.handle_input(input).map_err(negotiation_error)?;
                }
                frame = self.channels.outgoing.recv() => { let Some(frame) = frame else { return Ok(()); }; self.write(frame)?; }
                command = self.channels.commands.recv(), if !self.channels.commands.is_closed() || !self.channels.commands.is_empty() => { if command.is_some() { return Err(MediaError::Unsupported); } }
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.session.engine.timeout)) => self.session.engine.rtc.handle_input(Input::Timeout(Instant::now())).map_err(negotiation_error)?,
                () = tokio::time::sleep_until(handshake_deadline), if !self.session.engine.rtc.is_connected() => return Err(MediaError::Transport("WebRTC handshake timed out".into())),
            }
        }
    }

    async fn close(&mut self) -> Result<(), MediaError> {
        self.decoder.clear();
        self.session.engine.rtc.close().map_err(negotiation_error)?;
        let _alive = self.drain()?;
        self.flush().await
    }
}

impl WebRtcSession {
    pub async fn run(self, channels: Channels) -> Result<WebRtcStats, MediaError> {
        let decoded_format = match self.codec {
            Codec::Pcmu => AudioFormat::Pcmu,
            Codec::Pcma => AudioFormat::Pcma,
            Codec::Opus => AudioFormat::Opus,
        };
        let encoder = Encoder::new(self.codec)?;
        let mut link = Link {
            session: self,
            channels,
            encoder,
            decoder: AudioDecoder::new(decoded_format)?,
            decoded_format,
            stats: WebRtcStats::default(),
            started: Instant::now(),
        };
        let result = link.serve().await;
        let cleanup = link.close().await;
        let stats = mem::take(&mut link.stats);
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(stats),
            (Err(primary), Err(cleanup)) => Err(failure(format!(
                "{primary}; WebRTC cleanup failed: {cleanup}"
            ))),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        }
    }
}
