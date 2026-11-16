use super::{
    negotiation_error, telephone::TelephoneTransport, transmit_sources::TransmitSources,
    WebRtcSession,
};
use crate::media_link::{Channels, DigitCommand, LinkStats};
use rusterisk_config::Codec;
use rusterisk_media::{
    AudioFormat, Frame, MediaError, RtpPacket, RtpReceiver, RtpSender, MAX_PACKET,
};
use std::{
    mem,
    time::{Duration, Instant},
};
use str0m::{
    net::{Protocol, Receive},
    rtp::RtpWrite,
    Event, Input, Output,
};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, Default)]
pub struct WebRtcStats {
    pub transport_connected: bool,
    pub audio: LinkStats,
}

fn format(codec: str0m::format::Codec) -> Option<AudioFormat> {
    match codec {
        str0m::format::Codec::PCMU => Some(AudioFormat::Pcmu),
        str0m::format::Codec::PCMA => Some(AudioFormat::Pcma),
        str0m::format::Codec::Opus => Some(AudioFormat::Opus),
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
        | str0m::format::Codec::Unknown => None,
        _ => None,
    }
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

struct Link {
    session: WebRtcSession,
    channels: Channels,
    sender: RtpSender,
    input: Option<AudioInput>,
    sources: TransmitSources,
    telephone: Option<TelephoneTransport>,
    stats: WebRtcStats,
    started: Instant,
}

struct AudioInput {
    format: AudioFormat,
    payload: u8,
    source: u32,
    receiver: RtpReceiver,
}

impl Link {
    fn discard_input(&mut self) {
        if let Some(input) = &mut self.input {
            input.receiver.discard_pending();
        }
    }

    fn receive(&mut self, data: str0m::rtp::RtpPacket) -> Result<(), MediaError> {
        if *self.channels.stop.borrow() {
            self.discard_input();
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        let mid = self.session.engine.mid.ok_or(MediaError::InvalidRoute)?;
        let media = self
            .session
            .engine
            .rtc
            .media(mid)
            .ok_or(MediaError::InvalidRoute)?;
        let parameters =
            self.session
                .engine
                .rtc
                .codec_config()
                .params()
                .iter()
                .find(|parameters| {
                    parameters.pt() == data.header.payload_type
                        && media.remote_pts().contains(&parameters.pt())
                });
        let Some(parameters) = parameters else {
            self.stats.audio.dropped += 1;
            return Ok(());
        };
        let payload = *data.header.payload_type;
        let source = *data.header.ssrc;
        let packet = RtpPacket::new(
            payload,
            data.header.sequence_number,
            data.header.timestamp,
            source,
            &data.payload,
        )?
        .with_marker(data.header.marker);
        if self
            .telephone
            .as_ref()
            .is_some_and(|telephone| telephone.accepts(payload))
        {
            if let Some(input) = &mut self.input {
                if input.receiver.skip(&packet).is_err() {
                    self.stats.audio.dropped += 1;
                }
            }
            let now = self.started.elapsed().as_millis() as u64;
            let telephone = self.telephone.as_mut().ok_or(MediaError::Unsupported)?;
            match telephone.receive(&packet, now) {
                Ok(Some(events)) => {
                    if !self.channels.direction.borrow().can_send() {
                        telephone.discard_received();
                        self.discard_input();
                        self.stats.audio.dropped += 1;
                        return Ok(());
                    }
                    self.stats.audio.packets_received += 1;
                    for tone in events.into_tones() {
                        self.channels
                            .digits
                            .try_send(tone)
                            .map_err(|_| MediaError::Capacity)?;
                    }
                }
                Ok(None) | Err(_) => self.stats.audio.dropped += 1,
            }
            return Ok(());
        }
        if !self.channels.direction.borrow().can_send() {
            self.discard_input();
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        let Some(format) = format(parameters.spec().codec) else {
            self.stats.audio.dropped += 1;
            return Ok(());
        };
        if self.input.as_ref().is_none_or(|input| {
            input.format != format || input.payload != payload || input.source != source
        }) {
            self.input = Some(AudioInput {
                format,
                payload,
                source,
                receiver: RtpReceiver::new(format, payload)?,
            });
        }
        let input = self.input.as_mut().ok_or(MediaError::InvalidRoute)?;
        match input.receiver.push(&packet) {
            Ok(true) => self.stats.audio.packets_received += 1,
            Ok(false) | Err(_) => self.stats.audio.dropped += 1,
        }
        Ok(())
    }

    fn playout(&mut self) -> Result<(), MediaError> {
        self.telephone_tick()?;
        let Some(input) = &mut self.input else {
            return Ok(());
        };
        let frame = match input.receiver.pop() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(error) => {
                input.receiver.discard_pending();
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
        };
        if *self.channels.stop.borrow() || !self.channels.direction.borrow().can_send() {
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        match self.channels.incoming.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => self.stats.audio.dropped += 1,
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(MediaError::UnknownStream),
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
        let now = Instant::now();
        let packet = self
            .sender
            .packet_at(frame, now.duration_since(self.started).as_millis() as u64)?;
        self.write_packet(&packet, now)
    }

    fn write_packet(&mut self, packet: &RtpPacket, wallclock: Instant) -> Result<(), MediaError> {
        let source = packet.ssrc().into();
        if !self.sources.contains(packet.ssrc()) {
            let mid = self.session.engine.mid.ok_or(MediaError::InvalidRoute)?;
            if self.session.engine.rtc.media(mid).is_none() {
                return Err(MediaError::InvalidRoute);
            }
            self.sources.register(packet.ssrc())?;
            self.session
                .engine
                .rtc
                .direct_api()
                .declare_stream_tx(source, None, mid, None);
            if !self.drain()? {
                return Err(MediaError::UnknownStream);
            }
        }
        let sequence = self.sources.sequence(packet.ssrc(), packet.sequence())?;
        self.session
            .engine
            .rtc
            .direct_api()
            .stream_tx(&source)
            .ok_or(MediaError::InvalidRoute)?
            .write_rtp(
                RtpWrite::new(
                    packet.payload_type().into(),
                    sequence,
                    packet.timestamp(),
                    wallclock,
                    packet.payload().to_vec(),
                )
                .marker(packet.marker()),
            );
        self.stats.audio.packets_sent += 1;
        Ok(())
    }

    fn completed(&self, token: rusterisk_core::DtmfToken) -> Result<(), MediaError> {
        self.channels
            .completed
            .try_send(token)
            .map_err(|_| MediaError::Capacity)
    }

    fn finish_command(&self, command: DigitCommand) -> Result<(), MediaError> {
        match command {
            DigitCommand::Sequence(command) => self.completed(command.token),
            DigitCommand::Tone(_) => Ok(()),
        }
    }

    fn direction_changed(&mut self) -> Result<(), MediaError> {
        self.discard_input();
        if !self.channels.direction.borrow().can_send() {
            if let Some(telephone) = &mut self.telephone {
                telephone.discard_received();
            }
        }
        if !self.channels.direction.borrow().can_receive() {
            if let Some(token) = self.telephone.as_mut().and_then(TelephoneTransport::cancel) {
                self.completed(token)?;
            }
            for _ in 0..8 {
                match self.channels.commands.try_recv() {
                    Ok(command) => self.finish_command(command)?,
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => break,
                }
            }
        }
        Ok(())
    }

    fn telephone_tick(&mut self) -> Result<(), MediaError> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.started).as_millis() as u64;
        let Some(telephone) = &mut self.telephone else {
            return Ok(());
        };
        for tone in telephone.expire(elapsed)? {
            if self.channels.direction.borrow().can_send() {
                self.channels
                    .digits
                    .try_send(tone)
                    .map_err(|_| MediaError::Capacity)?;
            }
        }
        if !self.channels.direction.borrow().can_receive() {
            if let Some(token) = telephone.cancel() {
                self.completed(token)?;
            }
            return Ok(());
        }
        if !self.session.engine.rtc.is_connected() {
            return Ok(());
        }
        let output = telephone.poll(elapsed, &mut self.sender)?;
        let timestamp = telephone.timestamp_at(elapsed, &self.sender);
        let rate = telephone.rate();
        for packet in output.packets {
            let age = timestamp.wrapping_sub(packet.timestamp());
            if age >= 1 << 31 {
                return Err(MediaError::InvalidPacket);
            }
            let age = Duration::from_nanos(u64::from(age) * 1_000_000_000 / u64::from(rate));
            let wallclock = now.checked_sub(age).ok_or(MediaError::InvalidPacket)?;
            self.write_packet(&packet, wallclock)?;
        }
        if let Some(token) = output.completed {
            self.completed(token)?;
        }
        Ok(())
    }

    async fn serve(&mut self) -> Result<(), MediaError> {
        let mut bytes = [0; MAX_PACKET + 1];
        let handshake_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut playout = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(40),
            Duration::from_millis(20),
        );
        playout.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                changed = self.channels.direction.changed() => { if changed.is_err() { return Ok(()); } self.direction_changed()?; }
                result = self.session.engine.sockets.data().recv_from(&mut bytes) => {
                    let (size, source) = result.map_err(failure)?;
                    if size > MAX_PACKET { self.stats.audio.dropped += 1; continue; }
                    let contents = match bytes[..size].try_into() { Ok(contents) => contents, Err(_) => { self.stats.audio.dropped += 1; continue; } };
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: self.session.engine.sockets.local_addr().map_err(failure)?, contents });
                    if !self.session.engine.rtc.accepts(&input) { self.stats.audio.dropped += 1; continue; }
                    self.session.engine.rtc.handle_input(input).map_err(negotiation_error)?;
                }
                frame = self.channels.outgoing.recv() => { let Some(frame) = frame else { return Ok(()); }; self.write(frame)?; }
                _instant = playout.tick() => self.playout()?,
                command = self.channels.commands.recv(), if self.telephone.as_ref().is_none_or(TelephoneTransport::idle) && (!self.channels.commands.is_closed() || !self.channels.commands.is_empty()) => {
                    if let Some(command) = command {
                        if !self.channels.direction.borrow().can_receive() { self.finish_command(command)?; }
                        else { self.telephone.as_mut().ok_or(MediaError::Unsupported)?.admit(command)?; }
                    }
                }
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.session.engine.timeout)) => self.session.engine.rtc.handle_input(Input::Timeout(Instant::now())).map_err(negotiation_error)?,
                () = tokio::time::sleep_until(handshake_deadline), if !self.session.engine.rtc.is_connected() => return Err(MediaError::Transport("WebRTC handshake timed out".into())),
            }
        }
    }

    async fn close(&mut self) -> Result<(), MediaError> {
        self.discard_input();
        if let Some(telephone) = &mut self.telephone {
            let _cancelled = telephone.cancel();
        }
        self.session.engine.rtc.close().map_err(negotiation_error)?;
        let _alive = self.drain()?;
        self.flush().await
    }
}

impl WebRtcSession {
    pub async fn run(self, channels: Channels) -> Result<WebRtcStats, MediaError> {
        let format = match self.codec {
            Codec::Pcmu => AudioFormat::Pcmu,
            Codec::Pcma => AudioFormat::Pcma,
            Codec::Opus => AudioFormat::Opus,
        };
        let mut session = self;
        let mid = session.engine.mid.ok_or(MediaError::InvalidRoute)?;
        let source = session
            .engine
            .rtc
            .direct_api()
            .stream_tx_by_mid(mid, None)
            .ok_or(MediaError::InvalidRoute)?
            .ssrc();
        let mut seed = [0; 10];
        getrandom::fill(&mut seed).map_err(failure)?;
        seed[..4].copy_from_slice(&(*source).to_be_bytes());
        let sender = RtpSender::new(format, *session.payload, seed)?;
        let telephone = session
            .telephone()
            .map(|format| TelephoneTransport::new(&sender, *source, format, &session.telephones))
            .transpose()?;
        let mut sources = TransmitSources::new();
        sources.register(*source)?;
        let mut link = Link {
            session,
            channels,
            sender,
            input: None,
            sources,
            telephone,
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
