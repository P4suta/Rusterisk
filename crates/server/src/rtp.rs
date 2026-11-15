use rusterisk_core::{DtmfCommand, DtmfToken, DtmfTone, PeerMediaDirection};
use rusterisk_media::{
    AudioFormat, Frame, MediaError, RtpPacket, RtpReceiver, RtpSender, SourceReport,
    TelephoneFormat, TelephoneReceiver, TelephoneSender, MAX_PACKET,
};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

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
}

#[derive(Clone, Copy, Debug)]
pub struct RtpSession {
    pub remote: SocketAddr,
    pub format: AudioFormat,
    pub payload_type: u8,
    pub telephone: Option<TelephoneFormat>,
}

pub struct RtpChannels {
    pub incoming: mpsc::Sender<Frame>,
    pub digits: mpsc::Sender<DtmfTone>,
    pub outgoing: mpsc::Receiver<Frame>,
    pub commands: mpsc::Receiver<RtpDigitCommand>,
    pub completed: mpsc::Sender<DtmfToken>,
    pub stop: watch::Receiver<bool>,
    pub direction: watch::Receiver<PeerMediaDirection>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkStats {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub dropped: u64,
}

pub enum RtpDigitCommand {
    Tone(DtmfTone),
    Sequence(DtmfCommand),
}

pub struct RtpLink {
    sockets: SocketPair,
    session: RtpSession,
    channels: RtpChannels,
    receiver: RtpReceiver,
    sender: RtpSender,
    telephone: Option<TelephoneTransport>,
}

struct TelephoneTransport {
    receiver: TelephoneReceiver,
    sender: TelephoneSender,
    format: TelephoneFormat,
}

struct SentSource {
    report: SourceReport,
    rate: u32,
    sampled: tokio::time::Instant,
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

async fn send(socket: &UdpSocket, bytes: &[u8], peer: SocketAddr) -> Result<(), MediaError> {
    let sent = tokio::time::timeout(Duration::from_millis(20), socket.send_to(bytes, peer))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    if sent != bytes.len() {
        return Err(MediaError::Transport("incomplete datagram".into()));
    }
    Ok(())
}

fn report_delay(initial: bool) -> Result<Duration, MediaError> {
    let mut entropy = [0; 2];
    getrandom::fill(&mut entropy).map_err(failure)?;
    let random = f64::from(u16::from_be_bytes(entropy)) / 65536.0 + 0.5;
    Ok(Duration::from_secs_f64(
        if initial { 2.5 } else { 5.0 } * random / 1.21828,
    ))
}

impl RtpLink {
    pub fn new(
        sockets: SocketPair,
        session: RtpSession,
        channels: RtpChannels,
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
        Ok(Self {
            telephone,
            receiver: RtpReceiver::new(session.format, session.payload_type)?,
            sender,
            sockets,
            session,
            channels,
        })
    }

    pub async fn run(mut self) -> Result<LinkStats, MediaError> {
        let period = Duration::from_millis(20);
        let mut playout =
            tokio::time::interval_at(tokio::time::Instant::now() + period * 2, period);
        playout.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut report_at = tokio::time::Instant::now() + report_delay(true)?;
        let mut data = [0; MAX_PACKET + 1];
        let mut control = [0; MAX_PACKET + 1];
        let mut stats = LinkStats::default();
        let started = tokio::time::Instant::now();
        let mut sources = Vec::with_capacity(2);
        let mut active: Option<RtpDigitCommand> = None;
        let control_peer =
            SocketAddr::new(self.session.remote.ip(), self.session.remote.port() + 1);
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
                    self.receiver.discard_pending();
                    if !self.channels.direction.borrow().can_send() {
                        if let Some(telephone) = &mut self.telephone {
                            telephone.receiver = TelephoneReceiver::new(telephone.format.payload_type(), telephone.format.rate())?;
                        }
                    }
                }
                packet = self.sockets.data.recv_from(&mut data) => {
                    let (size, peer) = packet.map_err(failure)?;
                    if peer != self.session.remote || size > MAX_PACKET || !self.channels.direction.borrow().can_send() { stats.dropped += 1; continue; }
                    let now = started.elapsed().as_millis() as u64;
                    match RtpPacket::parse(&data[..size]).and_then(|packet| self.receive(&packet, now)) {
                        Ok(true) => stats.packets_received += 1,
                        Ok(false) | Err(_) => stats.dropped += 1,
                    }
                }
                packet = self.sockets.control.recv_from(&mut control) => {
                    let (size, peer) = packet.map_err(failure)?;
                    if peer != control_peer || size > MAX_PACKET || rusterisk_media::inspect_rtcp(&control[..size]).is_err() { stats.dropped += 1; }
                }
                frame = self.channels.outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    if !self.channels.direction.borrow().can_receive() { continue; }
                    let sampled = tokio::time::Instant::now();
                    let now = sampled.duration_since(started).as_millis() as u64;
                    let packet = self.sender.packet_at(frame, now)?;
                    send(&self.sockets.data, &packet.encode()?, self.session.remote).await?;
                    record(&mut sources, &packet, self.session.format.rate(), packet.timestamp(), sampled)?;
                    stats.packets_sent += 1;
                }
                command = self.channels.commands.recv(), if active.is_none() && self.telephone.is_some() && (!self.channels.commands.is_closed() || !self.channels.commands.is_empty()) => {
                    active = command;
                }
                _instant = playout.tick() => {
                    if let Some(telephone) = &mut self.telephone {
                        let sampled = tokio::time::Instant::now();
                        let now = sampled.duration_since(started).as_millis() as u64;
                        if let Some(tone) = telephone.receiver.expire(now)? {
                            if self.channels.direction.borrow().can_send() {
                                self.channels.digits.try_send(tone).map_err(|_| MediaError::Capacity)?;
                            }
                        }
                        for packet in telephone.sender.poll(now, &mut self.sender)? {
                            if !self.channels.direction.borrow().can_receive() { continue; }
                            send(&self.sockets.data, &packet.encode()?, self.session.remote).await?;
                            let timestamp = telephone.sender.timestamp_at(now, &self.sender);
                            record(&mut sources, &packet, telephone.format.rate(), timestamp, sampled)?;
                            stats.packets_sent += 1;
                        }
                        if let Some(command) = &mut active {
                            match command {
                                RtpDigitCommand::Tone(tone) => {
                                    if telephone.sender.ready() {
                                        telephone.sender.start(*tone, now, &self.sender)?;
                                        active = None;
                                    }
                                }
                                RtpDigitCommand::Sequence(command) => {
                                    let mut sequence = command.sequence;
                                    let tone = sequence.advance(now).map_err(|error| MediaError::Transport(format!("digit schedule: {error:?}")))?;
                                    if tone.is_none() || telephone.sender.ready() {
                                        if let Some(tone) = tone { telephone.sender.start(tone, now, &self.sender)?; }
                                        command.sequence = sequence;
                                        if sequence.finished() {
                                            self.channels.completed.try_send(command.token).map_err(|_| MediaError::Capacity)?;
                                            active = None;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    match self.receiver.pop() {
                        Ok(Some(frame)) if self.channels.direction.borrow().can_send() => match self.channels.incoming.try_send(frame) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => stats.dropped += 1,
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        },
                        Ok(Some(_)) | Ok(None) => {}
                        Err(_) => stats.dropped += 1,
                    }
                }
                () = tokio::time::sleep_until(report_at) => {
                    if !sources.is_empty() {
                        let (reports, ntp) = reports(&sources)?;
                        let bytes = rusterisk_media::sender_reports(&reports, ntp, "rusterisk")?;
                        send(&self.sockets.control, &bytes, control_peer).await?;
                    }
                    report_at = tokio::time::Instant::now() + report_delay(false)?;
                }
            }
        }
        if !sources.is_empty() {
            let (reports, ntp) = reports(&sources)?;
            let bytes = rusterisk_media::goodbye_reports(&reports, ntp, "rusterisk")?;
            send(&self.sockets.control, &bytes, control_peer).await?;
        }
        Ok(stats)
    }

    fn receive(&mut self, packet: &RtpPacket, now: u64) -> Result<bool, MediaError> {
        if self
            .session
            .telephone
            .is_some_and(|format| packet.payload_type() == format.payload_type())
        {
            let telephone = self.telephone.as_mut().ok_or(MediaError::Unsupported)?;
            for tone in telephone.receiver.push(packet, now)?.into_tones() {
                self.channels
                    .digits
                    .try_send(tone)
                    .map_err(|_| MediaError::Capacity)?;
            }
            self.receiver.skip(packet)?;
            Ok(true)
        } else {
            self.receiver.push(packet)
        }
    }
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
