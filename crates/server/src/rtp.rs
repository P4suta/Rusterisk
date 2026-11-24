pub use crate::media_link::{Channels as RtpChannels, DigitCommand as RtpDigitCommand, LinkStats};
use rusterisk_core::{DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::{
    AudioFormat, MediaError, RtpPacket, RtpReceiver, RtpSender, SourceReport, TelephoneFormat,
    TelephoneReceiver, TelephoneSender, MAX_PACKET,
};
use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::time::Instant;
#[cfg(test)]
mod control_tests;

pub struct SocketPair {
    data: UdpSocket,
    control: UdpSocket,
}

impl SocketPair {
    pub fn bind(ip: IpAddr, start: u16, end: u16) -> Result<Self, io::Error> {
        let ephemeral = start == 0 && end == 0;
        if !ephemeral && (start == 0 || !start.is_multiple_of(2) || start >= end) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid media port range",
            ));
        }
        let attempts = if ephemeral {
            128
        } else {
            (usize::from(end - start) / 2 + 1).min(256)
        };
        for index in 0..attempts {
            let port = if ephemeral {
                0
            } else {
                start + index as u16 * 2
            };
            let data = match std::net::UdpSocket::bind(SocketAddr::new(ip, port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            let local = data.local_addr()?;
            if !local.port().is_multiple_of(2) {
                continue;
            }
            let Some(control_port) = local.port().checked_add(1) else {
                continue;
            };
            if !ephemeral && control_port > end {
                continue;
            }
            let control = match std::net::UdpSocket::bind(SocketAddr::new(ip, control_port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            data.set_nonblocking(true)?;
            control.set_nonblocking(true)?;
            return Ok(Self {
                data: UdpSocket::from_std(data)?,
                control: UdpSocket::from_std(control)?,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "media port capacity exceeded",
        ))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.data.local_addr()
    }

    pub fn control_addr(&self) -> Result<SocketAddr, io::Error> {
        self.control.local_addr()
    }

    pub fn data(&self) -> &UdpSocket {
        &self.data
    }
    pub fn control(&self) -> &UdpSocket {
        &self.control
    }

    // Takes the sockets out of the runtime's reactor, for a leg that reads them without waiting.
    fn into_std(self) -> io::Result<(std::net::UdpSocket, std::net::UdpSocket)> {
        Ok((self.data.into_std()?, self.control.into_std()?))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RtpSession {
    pub remote: SocketAddr,
    pub format: AudioFormat,
    pub payload_type: u8,
    pub telephone: Option<TelephoneFormat>,
}

// The datagrams that a leg reads from each socket in one turn, which bounds a flood's share of the turn.
const TURN_DATAGRAMS: usize = 16;
// The digit commands that wait behind the one that a leg sends.
const DIGIT_COMMANDS: usize = 8;

// A Real-time Transport Protocol leg that the media task runs in its own turn.
pub struct RtpLeg {
    measurements: Measurements,
    data: std::net::UdpSocket,
    control: std::net::UdpSocket,
    control_peer: SocketAddr,
    session: RtpSession,
    receiver: RtpReceiver,
    sender: RtpSender,
    presentation: Option<PresentationClock>,
    telephone: Option<TelephoneTransport>,
    direction: PeerMediaDirection,
    started: Instant,
    report_at: Instant,
    sources: Vec<SentSource>,
    active: Option<RtpDigitCommand>,
    commands: VecDeque<RtpDigitCommand>,
    stats: LinkStats,
}

#[derive(Default, Clone)]
pub struct Measurements {
    pub local: rusterisk_api::ServerPacketCounts,
    pub peer: Option<rusterisk_api::PeerReception>,
}

#[derive(Clone, Copy)]
struct PresentationClock {
    observed: u64,
    next: u64,
}

impl PresentationClock {
    fn advance(previous: Option<Self>, now: u64) -> Result<(u64, Self), MediaError> {
        if previous.is_some_and(|previous| now < previous.observed) {
            return Err(MediaError::Transport(
                "Audio presentation clock moved backwards".into(),
            ));
        }
        let elapsed = now / 20 * 20;
        let timestamp = previous.map_or(elapsed, |previous| elapsed.max(previous.next));
        let next = timestamp.checked_add(20).ok_or(MediaError::Capacity)?;
        Ok((
            timestamp,
            Self {
                observed: now,
                next,
            },
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlReception {
    Accepted,
    Rejected,
}

struct TelephoneTransport {
    receiver: TelephoneReceiver,
    sender: TelephoneSender,
    format: TelephoneFormat,
}

struct SentSource {
    first_sequence: u16,
    last_sequence: u64,
    report: SourceReport,
    rate: u32,
    sampled: tokio::time::Instant,
}

impl SentSource {
    fn covers_reported_sequence(&self, sequence: u32) -> bool {
        let sequence = u64::from(sequence);
        let earliest = if sequence < u64::from(self.first_sequence) {
            sequence + (1 << 16)
        } else {
            sequence
        };
        earliest <= self.last_sequence
    }
}

fn record(
    sources: &mut Vec<SentSource>,
    packet: &RtpPacket,
    rate: u32,
    timestamp: u32,
    sampled: tokio::time::Instant,
) -> Result<(), MediaError> {
    let mut source = if let Some(index) = sources
        .iter()
        .position(|source| source.report.ssrc == packet.ssrc())
    {
        sources.remove(index)
    } else {
        if sources.len() >= 2 {
            return Err(MediaError::Capacity);
        }
        SentSource {
            first_sequence: packet.sequence(),
            last_sequence: u64::from(packet.sequence()),
            report: SourceReport {
                ssrc: packet.ssrc(),
                timestamp,
                packets: 0,
                octets: 0,
            },
            rate,
            sampled,
        }
    };
    source.report.timestamp = timestamp;
    let advance = packet.sequence().wrapping_sub(source.last_sequence as u16);
    source.last_sequence = source.last_sequence.saturating_add(u64::from(advance));
    source.report.packets = source.report.packets.wrapping_add(1);
    source.report.octets = source
        .report
        .octets
        .wrapping_add(packet.payload().len() as u32);
    source.sampled = sampled;
    sources.insert(0, source);
    Ok(())
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

// A datagram that a non-blocking socket received, or none when nothing waits.
fn received(
    result: io::Result<(usize, SocketAddr)>,
) -> Result<Option<(usize, SocketAddr)>, MediaError> {
    match result {
        Ok(received) => Ok(Some(received)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(failure(error)),
    }
}

// Sends a datagram without waiting, and reports false when the socket has no room for it.
fn transmit(
    socket: &std::net::UdpSocket,
    bytes: &[u8],
    peer: SocketAddr,
) -> Result<bool, MediaError> {
    match socket.send_to(bytes, peer) {
        Ok(sent) if sent == bytes.len() => Ok(true),
        Ok(_) => Err(MediaError::Transport("incomplete datagram".into())),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(failure(error)),
    }
}

// Sends a control report, which a full socket or a failure drops without ending the leg.
fn transmit_control(
    socket: &std::net::UdpSocket,
    bytes: &[u8],
    peer: SocketAddr,
    stats: &mut LinkStats,
) {
    match transmit(socket, bytes, peer) {
        Ok(true) => {}
        Ok(false) => {
            stats.dropped += 1;
            tracing::debug!(name: "control_report_dropped", "control report found no socket room");
        }
        Err(error) => {
            stats.dropped += 1;
            tracing::debug!(name: "control_report_dropped", %error, "control report delivery failed");
        }
    }
}

fn report_delay(initial: bool) -> Result<Duration, MediaError> {
    let mut entropy = [0; 2];
    getrandom::fill(&mut entropy).map_err(failure)?;
    let random = f64::from(u16::from_be_bytes(entropy)) / 65536.0 + 0.5;
    Ok(Duration::from_secs_f64(
        if initial { 2.5 } else { 5.0 } * random / 1.21828,
    ))
}

impl RtpLeg {
    pub fn new(
        sockets: SocketPair,
        session: RtpSession,
        direction: PeerMediaDirection,
    ) -> Result<Self, MediaError> {
        if session.remote.port() == 0 || session.remote.port() == u16::MAX {
            return Err(MediaError::InvalidPacket);
        }
        if session
            .telephone
            .is_some_and(|format| format.payload_type() == session.payload_type)
        {
            return Err(MediaError::InvalidPacket);
        }
        let mut seed = [0; 10];
        getrandom::fill(&mut seed).map_err(failure)?;
        let sender = RtpSender::new(session.format, session.payload_type, seed)?;
        let telephone = session
            .telephone
            .map(|format| {
                getrandom::fill(&mut seed).map_err(failure)?;
                Ok::<_, MediaError>(TelephoneTransport {
                    receiver: TelephoneReceiver::new(format.payload_type(), format.rate())?,
                    sender: TelephoneSender::with_format(&sender, format, seed)?,
                    format,
                })
            })
            .transpose()?;
        let (data, control) = sockets.into_std().map_err(failure)?;
        let started = Instant::now();
        Ok(Self {
            telephone,
            measurements: Measurements::default(),
            receiver: RtpReceiver::new(session.format, session.payload_type)?,
            sender,
            presentation: None,
            control_peer: SocketAddr::new(session.remote.ip(), session.remote.port() + 1),
            data,
            control,
            session,
            direction,
            started,
            report_at: started + report_delay(true)?,
            sources: Vec::with_capacity(2),
            active: None,
            commands: VecDeque::with_capacity(DIGIT_COMMANDS),
            stats: LinkStats::default(),
        })
    }

    // Milliseconds since the leg started, which its packet clocks count from.
    fn elapsed(&self, sampled: Instant) -> u64 {
        sampled.duration_since(self.started).as_millis() as u64
    }

    fn audio_packet(
        &mut self,
        frame: rusterisk_media::Frame,
        now: u64,
    ) -> Result<RtpPacket, MediaError> {
        if frame.samples().len() as u64 * 1000 != u64::from(frame.rate()) * 20 {
            return Err(MediaError::InvalidFrame);
        }
        let (timestamp, clock) = PresentationClock::advance(self.presentation, now)?;
        let packet = self.sender.packet_at(frame, timestamp)?;
        self.presentation = Some(clock);
        Ok(packet)
    }

    pub fn measurements(&self) -> Measurements {
        self.measurements.clone()
    }

    #[cfg(test)]
    pub(crate) fn measurements_mut(&mut self) -> &mut Measurements {
        &mut self.measurements
    }

    #[cfg(test)]
    pub(crate) fn local_addr(&self) -> Result<SocketAddr, MediaError> {
        self.data.local_addr().map_err(failure)
    }

    // Swaps the audio socket, so that a test can make the leg's sends fail.
    #[cfg(test)]
    pub(crate) fn replace_data_socket(&mut self, socket: std::net::UdpSocket) {
        self.data = socket;
    }

    fn received_control(&mut self, bytes: &[u8]) -> Result<(), MediaError> {
        let sources = &self.sources;
        let report = rusterisk_media::reception_report(bytes, |ssrc, sequence| {
            sources.iter().any(|source| {
                source.report.ssrc == ssrc && source.covers_reported_sequence(sequence)
            })
        })?;
        if let Some(report) = report {
            self.measurements.peer = Some(rusterisk_api::PeerReception {
                source: rusterisk_api::PeerReportSource::PeerRtcp,
                observed_at: crate::utc_timestamps::now_millis(),
                reporter_ssrc: report.reporter_ssrc,
                reported_ssrc: report.reported_ssrc,
                extended_highest_sequence: report.extended_highest_sequence,
                cumulative_lost: report.cumulative_lost,
            });
        }
        Ok(())
    }

    // Changes the peer direction and drops the audio and digits that the old direction admitted.
    pub fn set_direction(&mut self, direction: PeerMediaDirection) {
        self.direction = direction;
        self.receiver.discard_pending();
        if !direction.can_send() {
            if let Some(telephone) = &mut self.telephone {
                telephone.receiver.discard_pending();
            }
        }
    }

    // Queues a digit command behind the one that the leg sends now.
    pub fn queue_digits(&mut self, command: RtpDigitCommand) -> Result<(), MediaError> {
        if self.commands.len() >= DIGIT_COMMANDS {
            return Err(MediaError::Capacity);
        }
        self.commands.push_back(command);
        Ok(())
    }

    // Reads the datagrams that arrived since the last turn, at most one reorder window from each socket.
    pub fn receive(
        &mut self,
        scratch: &mut [u8; MAX_PACKET + 1],
        tones: &mut Vec<DtmfTone>,
        mut receipt: impl FnMut(ControlReception) -> Result<(), MediaError>,
    ) -> Result<(), MediaError> {
        for _ in 0..TURN_DATAGRAMS {
            let Some((size, peer)) = received(self.data.recv_from(scratch))? else {
                break;
            };
            if peer != self.session.remote || size > MAX_PACKET {
                self.stats.dropped += 1;
                continue;
            }
            let now = self.elapsed(Instant::now());
            match RtpPacket::parse(&scratch[..size]).and_then(|packet| {
                self.measurements.local.rxcount = self.measurements.local.rxcount.saturating_add(1);
                self.accept(&packet, now, tones)
            }) {
                Ok(true) => self.stats.packets_received += 1,
                Ok(false) | Err(_) => self.stats.dropped += 1,
            }
        }
        for _ in 0..TURN_DATAGRAMS {
            let Some((size, peer)) = received(self.control.recv_from(scratch))? else {
                break;
            };
            let reception = if peer != self.control_peer
                || size > MAX_PACKET
                || self.received_control(&scratch[..size]).is_err()
            {
                self.stats.dropped += 1;
                ControlReception::Rejected
            } else {
                ControlReception::Accepted
            };
            receipt(reception)?;
        }
        Ok(())
    }

    fn accept(
        &mut self,
        packet: &RtpPacket,
        now: u64,
        tones: &mut Vec<DtmfTone>,
    ) -> Result<bool, MediaError> {
        if self
            .session
            .telephone
            .is_some_and(|format| packet.payload_type() == format.payload_type())
        {
            let telephone = self.telephone.as_mut().ok_or(MediaError::Unsupported)?;
            let events = telephone.receiver.push(packet, now)?;
            if !self.direction.can_send() {
                telephone.receiver.discard_pending();
                return Ok(false);
            }
            tones.extend(events.into_tones());
            self.receiver.skip(packet)?;
            Ok(true)
        } else if self.direction.can_send() {
            self.receiver.push(packet)
        } else {
            Ok(false)
        }
    }

    // Advances telephone events, digit commands, and control reports to this turn, and takes the next received frame.
    pub fn advance(&mut self, tones: &mut Vec<DtmfTone>) -> Result<Advanced, MediaError> {
        let mut advanced = Advanced {
            frame: None,
            completed: None,
        };
        if self.active.is_none() && self.telephone.is_some() {
            self.active = self.commands.pop_front();
        }
        let sampled = Instant::now();
        let now = self.elapsed(sampled);
        if let Some(telephone) = &mut self.telephone {
            if let Some(tone) = telephone.receiver.expire(now)? {
                if self.direction.can_send() {
                    tones.push(tone);
                }
            }
            for packet in telephone.sender.poll(now, &mut self.sender)? {
                if !self.direction.can_receive() {
                    continue;
                }
                if transmit(&self.data, &packet.encode()?, self.session.remote)? {
                    self.measurements.local.txcount =
                        self.measurements.local.txcount.saturating_add(1);
                    let timestamp = telephone.sender.timestamp_at(now, &self.sender);
                    record(
                        &mut self.sources,
                        &packet,
                        telephone.format.rate(),
                        timestamp,
                        sampled,
                    )?;
                    self.stats.packets_sent += 1;
                } else {
                    self.stats.dropped += 1;
                }
            }
            if let Some(command) = &mut self.active {
                match command {
                    RtpDigitCommand::Tone(tone) => {
                        if telephone.sender.ready() {
                            telephone.sender.start(*tone, now, &self.sender)?;
                            self.active = None;
                        }
                    }
                    RtpDigitCommand::Sequence(command) => {
                        let mut sequence = command.sequence;
                        let tone = sequence.advance(now).map_err(|error| {
                            MediaError::Transport(format!("digit schedule: {error:?}"))
                        })?;
                        if tone.is_none() || telephone.sender.ready() {
                            if let Some(tone) = tone {
                                telephone.sender.start(tone, now, &self.sender)?;
                            }
                            command.sequence = sequence;
                            if sequence.finished() {
                                advanced.completed = Some(command.token);
                                self.active = None;
                            }
                        }
                    }
                }
            }
        }
        match self.receiver.pop() {
            Ok(Some(frame)) if self.direction.can_send() => advanced.frame = Some(frame),
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => self.stats.dropped += 1,
        }
        if sampled >= self.report_at {
            if !self.sources.is_empty() {
                let (reports, ntp) = reports(&self.sources)?;
                let bytes = rusterisk_media::sender_reports(&reports, ntp, "rusterisk")?;
                transmit_control(&self.control, &bytes, self.control_peer, &mut self.stats);
            }
            self.report_at = sampled + report_delay(false)?;
        }
        Ok(advanced)
    }

    // Sends one mixed frame as the next audio packet.
    pub fn send(&mut self, frame: rusterisk_media::Frame) -> Result<(), MediaError> {
        if !self.direction.can_receive() {
            return Ok(());
        }
        let sampled = Instant::now();
        let now = self.elapsed(sampled);
        let packet = self.audio_packet(frame, now)?;
        if !transmit(&self.data, &packet.encode()?, self.session.remote)? {
            self.stats.dropped += 1;
            return Ok(());
        }
        self.measurements.local.txcount = self.measurements.local.txcount.saturating_add(1);
        record(
            &mut self.sources,
            &packet,
            self.session.format.rate(),
            packet.timestamp(),
            sampled,
        )?;
        self.stats.packets_sent += 1;
        Ok(())
    }

    // Ends the leg with a goodbye report for each source that it sent.
    pub fn finish(mut self) -> LinkStats {
        if !self.sources.is_empty() {
            match reports(&self.sources).and_then(|(reports, ntp)| {
                rusterisk_media::goodbye_reports(&reports, ntp, "rusterisk")
            }) {
                Ok(bytes) => {
                    transmit_control(&self.control, &bytes, self.control_peer, &mut self.stats)
                }
                Err(error) => {
                    self.stats.dropped += 1;
                    tracing::debug!(name: "control_report_dropped", %error, "goodbye report failed");
                }
            }
        }
        self.stats
    }
}

// What one turn of a leg produced for the media task.
pub struct Advanced {
    pub frame: Option<rusterisk_media::Frame>,
    pub completed: Option<DtmfToken>,
}
fn reports(sources: &[SentSource]) -> Result<(Vec<SourceReport>, u64), MediaError> {
    let sampled = tokio::time::Instant::now();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(failure)?;
    let ntp = (wall.as_secs().wrapping_add(2_208_988_800) << 32)
        | ((u64::from(wall.subsec_nanos()) << 32) / 1_000_000_000);
    let reports = sources
        .iter()
        .map(|source| {
            let elapsed = sampled.duration_since(source.sampled).as_nanos()
                * u128::from(source.rate)
                / 1_000_000_000;
            SourceReport {
                timestamp: source.report.timestamp.wrapping_add(elapsed as u32),
                ..source.report
            }
        })
        .collect();
    Ok((reports, ntp))
}
