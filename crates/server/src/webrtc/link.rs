use super::{
    negotiation_error, telephone::TelephoneTransport, transmit_sources::TransmitSources,
    WebRtcSession,
};
use crate::media_link::{DigitCommand, LinkStats};
use rusterisk_core::{DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::{
    AudioFormat, Frame, MediaError, RtpPacket, RtpReceiver, RtpSender, MAX_PACKET,
};
use std::{
    collections::VecDeque,
    io, mem,
    time::{Duration, Instant},
};
use str0m::{
    net::{Protocol, Receive},
    rtp::RtpWrite,
    Event, Input, Output,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct WebRtcStats {
    pub transport_connected: bool,
    pub audio: LinkStats,
}

// The datagrams that a leg reads in one turn, which bounds a flood's share of the turn.
const TURN_DATAGRAMS: usize = 16;
// The digit commands that wait behind the one that a leg sends.
const DIGIT_COMMANDS: usize = 8;
// The received digits and finished commands that one turn can report.
const TURN_DIGITS: usize = 16;
// The refused handshake datagrams that a leg keeps, and the turns for which it keeps each one.
const HELD_DATAGRAMS: usize = 4;
const HELD_TURNS: u8 = 10;
// A leg that has not connected by this time after it starts fails.
const HANDSHAKE: Duration = Duration::from_secs(10);

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

// A WebRTC leg that the media task runs in its own turn.
// Its sockets stay registered with the runtime, whose reactor marks them readable between turns.
pub struct WebRtcLeg {
    session: WebRtcSession,
    sender: RtpSender,
    input: Option<AudioInput>,
    sources: TransmitSources,
    telephone: Option<TelephoneTransport>,
    stats: WebRtcStats,
    started: Instant,
    handshake: tokio::time::Instant,
    direction: PeerMediaDirection,
    commands: VecDeque<DigitCommand>,
    tones: Vec<DtmfTone>,
    finished: Vec<DtmfToken>,
    // The peer or the engine closed the session, and the media task ends the leg.
    closed: bool,
    // Handshake datagrams that the engine refused, which later turns offer again.
    held: Vec<Held>,
}

struct Held {
    bytes: Vec<u8>,
    source: std::net::SocketAddr,
    turns: u8,
}

struct AudioInput {
    format: AudioFormat,
    payload: u8,
    source: u32,
    receiver: RtpReceiver,
}

impl WebRtcLeg {
    pub fn new(session: WebRtcSession, direction: PeerMediaDirection) -> Result<Self, MediaError> {
        let format = session.audio_format();
        let mut session = session;
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
        Ok(Self {
            session,
            sender,
            input: None,
            sources,
            telephone,
            stats: WebRtcStats::default(),
            started: Instant::now(),
            handshake: tokio::time::Instant::now() + HANDSHAKE,
            direction,
            commands: VecDeque::with_capacity(DIGIT_COMMANDS),
            tones: Vec::with_capacity(TURN_DIGITS),
            finished: Vec::with_capacity(TURN_DIGITS),
            closed: false,
            held: Vec::new(),
        })
    }

    pub fn stats(&self) -> WebRtcStats {
        self.stats
    }

    fn discard_input(&mut self) {
        if let Some(input) = &mut self.input {
            input.receiver.discard_pending();
        }
    }

    fn tone(&mut self, tone: DtmfTone) -> Result<(), MediaError> {
        if self.tones.len() >= TURN_DIGITS {
            return Err(MediaError::Capacity);
        }
        self.tones.push(tone);
        Ok(())
    }

    fn receive_packet(&mut self, data: str0m::rtp::RtpPacket) -> Result<(), MediaError> {
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
                    if !self.direction.can_send() {
                        telephone.discard_received();
                        self.discard_input();
                        self.stats.audio.dropped += 1;
                        return Ok(());
                    }
                    self.stats.audio.packets_received += 1;
                    for tone in events.into_tones() {
                        self.tone(tone)?;
                    }
                }
                Ok(None) | Err(_) => self.stats.audio.dropped += 1,
            }
            return Ok(());
        }
        if !self.direction.can_send() {
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

    fn playout(&mut self) -> Result<Option<Frame>, MediaError> {
        self.telephone_tick()?;
        let Some(input) = &mut self.input else {
            return Ok(None);
        };
        let frame = match input.receiver.pop() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(None),
            Err(error) => {
                input.receiver.discard_pending();
                self.stats.audio.dropped += 1;
                return match error {
                    MediaError::InvalidFrame
                    | MediaError::InvalidPacket
                    | MediaError::Capacity
                    | MediaError::Codec(_) => Ok(None),
                    error @ (MediaError::InvalidRoute
                    | MediaError::UnknownStream
                    | MediaError::ServiceClosed
                    | MediaError::Unsupported
                    | MediaError::Transport(_)
                    | MediaError::Control(_)) => Err(error),
                };
            }
        };
        if !self.direction.can_send() {
            self.stats.audio.dropped += 1;
            return Ok(None);
        }
        Ok(Some(frame))
    }

    // Takes the engine's output up to its next deadline, and reports whether the engine stays open.
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
                        self.receive_packet(data)?;
                    }
                }
            }
        }
        Err(MediaError::Capacity)
    }

    // Sends the engine's datagrams without waiting, and drops one that finds no socket room.
    fn flush(&mut self) -> Result<(), MediaError> {
        for transmit in self.session.engine.transmits.drain(..) {
            if transmit.proto != Protocol::Udp {
                return Err(MediaError::Unsupported);
            }
            match self
                .session
                .engine
                .sockets
                .data()
                .try_send_to(&transmit.contents, transmit.destination)
            {
                Ok(size) if size == transmit.contents.len() => {}
                Ok(_) => return Err(MediaError::Transport("Incomplete WebRTC datagram".into())),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.stats.audio.dropped += 1;
                }
                Err(error) => return Err(failure(error)),
            }
        }
        Ok(())
    }

    // Lets the engine act on its output, and notes a closed session.
    fn settle(&mut self) -> Result<(), MediaError> {
        let open = self.drain()?;
        self.flush()?;
        self.closed |= !open;
        Ok(())
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    // Reads the datagrams that arrived since the last turn and advances the engine to its deadline.
    pub fn receive(&mut self, scratch: &mut [u8; MAX_PACKET + 1]) -> Result<(), MediaError> {
        // The engine learns the time at every turn, as it did at each of its own deadlines in a task.
        self.advance_engine()?;
        self.retry_held()?;
        for _ in 0..TURN_DATAGRAMS {
            let (size, source) = match self.session.engine.sockets.data().try_recv_from(scratch) {
                Ok(received) => received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(failure(error)),
            };
            if size > MAX_PACKET {
                self.stats.audio.dropped += 1;
                continue;
            }
            if !self.offer(&scratch[..size], source)? {
                self.hold(&scratch[..size], source);
            }
        }
        if !self.session.engine.rtc.is_connected() && tokio::time::Instant::now() >= self.handshake
        {
            return Err(MediaError::Transport("WebRTC handshake timed out".into()));
        }
        Ok(())
    }

    // Hands a datagram to the engine, and reports false when the engine refuses it.
    fn offer(&mut self, bytes: &[u8], source: std::net::SocketAddr) -> Result<bool, MediaError> {
        let Ok(contents) = bytes.try_into() else {
            self.stats.audio.dropped += 1;
            return Ok(true);
        };
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.session.engine.sockets.local_addr().map_err(failure)?,
                contents,
            },
        );
        if !self.session.engine.rtc.accepts(&input) {
            return Ok(false);
        }
        self.session
            .engine
            .rtc
            .handle_input(input)
            .map_err(negotiation_error)?;
        self.advance_engine()?;
        Ok(true)
    }

    // Keeps a refused datagram while the leg connects.
    // The engine can refuse a handshake datagram that arrives in the same turn as the check that admits its source.
    // A task that read each datagram on arrival gave the engine that time between them.
    fn hold(&mut self, bytes: &[u8], source: std::net::SocketAddr) {
        if self.session.engine.rtc.is_connected() || self.held.len() >= HELD_DATAGRAMS {
            self.stats.audio.dropped += 1;
            return;
        }
        self.held.push(Held {
            bytes: bytes.to_vec(),
            source,
            turns: 0,
        });
    }

    // Offers held datagrams again, and drops one that the engine still refuses after a few turns.
    fn retry_held(&mut self) -> Result<(), MediaError> {
        let held = mem::take(&mut self.held);
        for mut datagram in held {
            if self.offer(&datagram.bytes, datagram.source)? {
                continue;
            }
            datagram.turns += 1;
            if datagram.turns >= HELD_TURNS {
                self.stats.audio.dropped += 1;
            } else {
                self.held.push(datagram);
            }
        }
        Ok(())
    }

    // Advances telephone events and digit commands to this turn, and takes the next received frame.
    pub fn advance(
        &mut self,
        tones: &mut Vec<DtmfTone>,
        finished: &mut Vec<DtmfToken>,
    ) -> Result<Option<Frame>, MediaError> {
        if self.telephone.as_ref().is_none_or(TelephoneTransport::idle) {
            if let Some(command) = self.commands.pop_front() {
                if !self.direction.can_receive() {
                    self.finish_command(command)?;
                } else {
                    self.telephone
                        .as_mut()
                        .ok_or(MediaError::Unsupported)?
                        .admit(command)?;
                }
            }
        }
        let frame = self.playout()?;
        self.settle()?;
        tones.append(&mut self.tones);
        finished.append(&mut self.finished);
        Ok(frame)
    }

    // Sends one mixed frame as the next audio packet.
    pub fn send(&mut self, frame: Frame) -> Result<(), MediaError> {
        if !self.direction.can_receive() || !self.session.engine.rtc.is_connected() {
            self.stats.audio.dropped += 1;
            return Ok(());
        }
        self.advance_engine()?;
        let now = Instant::now();
        let packet = self
            .sender
            .packet_at(frame, now.duration_since(self.started).as_millis() as u64)?;
        self.write_packet(&packet, now)?;
        self.settle()
    }

    // Moves the engine's clock to now and lets it act on what falls due.
    fn advance_engine(&mut self) -> Result<(), MediaError> {
        self.session
            .engine
            .rtc
            .handle_input(Input::Timeout(Instant::now()))
            .map_err(negotiation_error)?;
        self.settle()
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

    fn completed(&mut self, token: DtmfToken) -> Result<(), MediaError> {
        if self.finished.len() >= TURN_DIGITS {
            return Err(MediaError::Capacity);
        }
        self.finished.push(token);
        Ok(())
    }

    fn finish_command(&mut self, command: DigitCommand) -> Result<(), MediaError> {
        match command {
            DigitCommand::Sequence(command) => self.completed(command.token),
            DigitCommand::Tone(_) => Ok(()),
        }
    }

    // Queues a digit command behind the one that the leg sends now.
    pub fn queue_digits(&mut self, command: DigitCommand) -> Result<(), MediaError> {
        if self.commands.len() >= DIGIT_COMMANDS {
            return Err(MediaError::Capacity);
        }
        self.commands.push_back(command);
        Ok(())
    }

    // Changes the peer direction, drops what the old direction admitted, and ends digits it can't send.
    pub fn set_direction(&mut self, direction: PeerMediaDirection) -> Result<(), MediaError> {
        self.direction = direction;
        self.discard_input();
        if !direction.can_send() {
            if let Some(telephone) = &mut self.telephone {
                telephone.discard_received();
            }
        }
        if !direction.can_receive() {
            if let Some(token) = self.telephone.as_mut().and_then(TelephoneTransport::cancel) {
                self.completed(token)?;
            }
            while let Some(command) = self.commands.pop_front() {
                self.finish_command(command)?;
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
        let mut expired = telephone.expire(elapsed)?;
        if self.direction.can_send() {
            for tone in expired.drain(..) {
                self.tone(tone)?;
            }
        }
        let Some(telephone) = &mut self.telephone else {
            return Ok(());
        };
        if !self.direction.can_receive() {
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

    // Ends the leg, closing its engine and sending what the close leaves.
    pub fn finish(mut self) -> WebRtcStats {
        self.discard_input();
        if let Some(telephone) = &mut self.telephone {
            let _cancelled = telephone.cancel();
        }
        let closed = self
            .session
            .engine
            .rtc
            .close()
            .map_err(negotiation_error)
            .and_then(|()| self.drain())
            .and_then(|_| self.flush());
        if let Err(error) = closed {
            tracing::debug!(%error, "WebRTC close failed");
        }
        mem::take(&mut self.stats)
    }
}
